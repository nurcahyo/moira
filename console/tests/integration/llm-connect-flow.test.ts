// The whole "Connect local vLLM" chain, end to end, behind a REAL session.
//
// ============================================================================
// WHAT IS REAL HERE
// ============================================================================
//
//   * a real OpenID Connect provider on a real TLS socket, and a real
//     authorization-code exchange against it (`tests/support/mock-idp.ts`);
//   * the shipped `createConsoleAuth` factory with the shipped plugin set, so
//     the cookie this test carries is one the console itself minted;
//   * `withConsoleSession` — NOT substituted. The route handlers below are
//     handed the real `ConsoleAuth` instance and the real resolved config, so
//     `consoleSessionCheck`, `resolveSessionProviderIn`, `readIdpSubject` and
//     `checkSession` all run against that cookie;
//   * the shipped route handlers, imported and called the way Next calls them;
//   * a real HTTP server answering `GET /v1/models`, on a real socket, which the
//     BFF reaches with its own `fetch`.
//
// MOCKED: Moira's HTTP surface, by `tests/support/moira-stub.ts`. What that
// costs is bounded and stated — the ORDER, the headers and the bodies are all
// asserted, and `tests/contract/openapi-contract.test.ts` re-derives every
// operation's shape from the committed spec on each run.
//
// ============================================================================
// WHY THIS IS NOT A PLAYWRIGHT SPEC
// ============================================================================
//
// There is no authenticated Playwright project in this repository, and the e2e
// environment forbids one rather than merely lacking it: `MOIRA_API_URL` is
// `https://moira.invalid`, `CONSOLE_DATABASE_URL` points at `db.invalid:1`, and
// no IdP runs inside it. `e2e/a11y.e2e.ts` records the same constraint and the
// same reversal condition.
//
// A browser spec would therefore have had to mint its own storage state, which
// is a test of the fixture rather than of the credential boundary. This drives
// the boundary itself. `e2e/llm-settings.e2e.ts` owns the half a browser can
// genuinely answer: that an unauthenticated visitor gets nothing.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { memoryAdapter } from "better-auth/adapters/memory";

import { POST as CONNECT } from "@/app/api/llm/connect-vllm/route";
import { GET as PROVIDERS_GET } from "@/app/api/llm/providers/route";
import { CONSOLE_OAUTH_PROVIDER_ID, type ResolvedAuthConfig } from "@/lib/auth-config";
import { createConsoleMemoryDatabase } from "@/lib/auth";
import { setConsoleApiDependenciesForTests } from "@/lib/console-api";
import { readConsoleEnv, type ConsoleEnv } from "@/lib/env";
import { MoiraClient } from "@/lib/moira-client";

import { createBrowserAgent } from "../support/browser-agent";
import { reserveConsolePort, startConsoleServer, type ConsoleServer } from "../support/console-server";
import { trustFixtureCa, untrustFixtureCa } from "../support/fixture-tls";
import { restoreDomWhatwgGlobals, useNativeWhatwgGlobals } from "../support/native-globals";
import { startMockIdp, type MockIdp } from "../support/mock-idp";
import {
  createMoiraStub,
  errorEnvelope,
  MOIRA_STUB_BASE_URL,
  type MoiraStub,
  type StubHandler,
} from "../support/moira-stub";

/* -------------------------------------------------------------------------- */
/* Fixtures                                                                   */
/* -------------------------------------------------------------------------- */

const CLIENT_ID = "moira-console.apps.mock-idp.test";
const CLIENT_SECRET = "mock-idp-client-secret-do-not-reuse";
const OPERATOR = {
  sub: "mock-idp-subject-llm-0f3a91",
  email: "operator@example.com",
  emailVerified: true,
  name: "Console Operator",
};

const PROVIDER_ID = "11111111-1111-4111-8111-111111111111";
const MODEL_ID = "22222222-2222-4222-8222-222222222222";
const CREDENTIAL_ID = "33333333-3333-4333-8333-333333333333";
const ROUTE_ID = "44444444-4444-4444-8444-444444444444";
const POLICY_ID = "55555555-5555-4555-8555-555555555555";

/** What the mock endpoint advertises, and what the chain must end up storing. */
const MODEL_KEY = "Qwen3-4B";

const PROVIDER_LIST = "GET /api/v1/admin/providers";
const PROVIDER_CREATE = "POST /api/v1/admin/providers";
const MODEL_LIST = `GET /api/v1/admin/providers/${PROVIDER_ID}/models`;
const MODEL_CREATE = `POST /api/v1/admin/providers/${PROVIDER_ID}/models`;
const CREDENTIAL_LIST = "GET /api/v1/admin/provider-credentials";
const CREDENTIAL_CREATE = "POST /api/v1/admin/provider-credentials";
const ROUTE_LIST = "GET /api/v1/admin/routes";
const POLICY_LIST = "GET /api/v1/admin/routing-policies";
const POLICY_CREATE = "POST /api/v1/admin/routing-policies";

const EMPTY_PAGE = { data: [], pagination: { has_more: false, next_cursor: null } };

function page(rows: readonly unknown[]) {
  return { data: rows, pagination: { has_more: false, next_cursor: null } };
}

function stamped(id: string, extra: Record<string, unknown>) {
  return {
    id,
    status: "active",
    metadata: {},
    created_at: "2026-08-04T00:00:00Z",
    updated_at: "2026-08-04T00:00:00Z",
    version: 1,
    ...extra,
  };
}

let vllmBaseUrl = "";

function moiraHandlers(overrides: Record<string, StubHandler> = {}): Record<string, StubHandler> {
  return {
    [PROVIDER_LIST]: () => ({ status: 200, body: EMPTY_PAGE }),
    [PROVIDER_CREATE]: () => ({
      status: 201,
      body: stamped(PROVIDER_ID, {
        provider_type: "open_ai_compatible",
        display_name: "Local endpoint",
        base_url: vllmBaseUrl,
      }),
    }),
    [MODEL_LIST]: () => ({ status: 200, body: EMPTY_PAGE }),
    [MODEL_CREATE]: () => ({
      status: 201,
      body: stamped(MODEL_ID, {
        provider_id: PROVIDER_ID,
        model_key: MODEL_KEY,
        capabilities: { text: true, streaming: true, tools: true },
      }),
    }),
    [CREDENTIAL_LIST]: () => ({ status: 200, body: EMPTY_PAGE }),
    [CREDENTIAL_CREATE]: () => ({
      status: 201,
      body: stamped(CREDENTIAL_ID, {
        provider_id: PROVIDER_ID,
        credential_type: "api_key",
        scope: { type: "global" },
        secret_fingerprint: "sha256:fingerprint-that-must-not-cross",
        masked_secret: "sk-****mask",
        priority: 0,
      }),
    }),
    [ROUTE_LIST]: () => ({
      status: 200,
      body: page([
        stamped(ROUTE_ID, {
          route_key: "general",
          display_name: "General",
          selection_strategy: "default",
        }),
      ]),
    }),
    [POLICY_LIST]: () => ({ status: 200, body: EMPTY_PAGE }),
    [POLICY_CREATE]: () => ({
      status: 201,
      body: stamped(POLICY_ID, {
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
      }),
    }),
    ...overrides,
  };
}

function envFor(consoleOrigin: string): ConsoleEnv {
  return readConsoleEnv({
    NODE_ENV: "test",
    MOIRA_API_URL: MOIRA_STUB_BASE_URL,
    CONSOLE_PUBLIC_ORIGIN: consoleOrigin,
    MOIRA_ADMIN_API_AUDIENCE: "moira-admin-api",
    BETTER_AUTH_SECRET: "fixture-better-auth-secret-at-least-32-chars",
    CONSOLE_SECRET_ENCRYPTION_KEY: Buffer.alloc(32, 7).toString("base64"),
  });
}

function configFor(idp: MockIdp, env: ConsoleEnv): ResolvedAuthConfig {
  return {
    providerId: CONSOLE_OAUTH_PROVIDER_ID,
    consoleIssuer: env.bffIssuerUrl,
    method: "generic_oidc",
    moiraProviderId: "99999999-9999-4999-8999-999999999999",
    moiraProviderVersion: 1,
    issuer: idp.issuer,
    discoveryUrl: idp.discoveryUrl,
    authorizationUrl: idp.authorizationUrl,
    tokenUrl: idp.tokenUrl,
    userInfoUrl: idp.userInfoUrl,
    clientId: CLIENT_ID,
    clientSecret: CLIENT_SECRET,
    scopes: ["openid", "email", "profile"],
    allowedEmailDomains: ["example.com"],
    trustedJwtIssuerId: "88888888-8888-4888-8888-888888888888",
  };
}

/* -------------------------------------------------------------------------- */
/* Harness                                                                    */
/* -------------------------------------------------------------------------- */

let idp: MockIdp;
let consoleServer: ConsoleServer;
let env: ConsoleEnv;
let config: ResolvedAuthConfig;
let vllm: ReturnType<typeof Bun.serve>;
let vllmRequests: string[] = [];
let signedInCookie = "";
let stub: MoiraStub;

/** Complete a real authorization-code exchange and return the session cookie. */
async function signIn(): Promise<string> {
  const agent = createBrowserAgent();
  const start = await agent.request(`${consoleServer.origin}/api/auth/sign-in/oauth2`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ providerId: CONSOLE_OAUTH_PROVIDER_ID, callbackURL: "/" }),
  });
  expect(start.status).toBe(200);
  const { url } = (await start.json()) as { url: string };
  await agent.navigate(url);
  const cookie = agent.cookieHeaderFor(consoleServer.origin);
  expect(cookie, "the sign-in flow produced no cookie").toContain("session_token");
  return cookie;
}

/**
 * Wire the route handlers to the REAL auth instance and the stub Moira.
 *
 * The session check is not substituted — see this file's header. `runtime`
 * hands back the instance the sign-in above used, so the cookie resolves for
 * real.
 */
function install(handlers: Record<string, StubHandler> = moiraHandlers()): void {
  stub = createMoiraStub(handlers);
  setConsoleApiDependenciesForTests({
    runtime: async () => ({
      ok: true,
      auth: consoleServer.auth,
      configs: [config],
      problems: [],
      stale: false,
    }),
    env: () => env,
    clientFor: () =>
      new MoiraClient({
        baseUrl: MOIRA_STUB_BASE_URL,
        systemKey: "sk_test_stub",
        fetch: stub.fetch,
      }),
  });
}

/** A request carrying (or not carrying) the real console session cookie. */
function request(body: unknown, cookie: string | null): Request {
  return new Request(`${consoleServer.origin}/api/llm/connect-vllm`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      ...(cookie === null ? {} : { cookie }),
    },
    body: JSON.stringify(body),
  });
}

beforeAll(async () => {
  // Server-side suite: happy-dom's Headers silently discards `Set-Cookie`.
  useNativeWhatwgGlobals();

  const port = reserveConsolePort();
  const consoleOrigin = `https://localhost:${port}`;
  trustFixtureCa(consoleOrigin);

  idp = await startMockIdp({ clientId: CLIENT_ID, clientSecret: CLIENT_SECRET, user: OPERATOR });
  trustFixtureCa(idp.origin);

  env = envFor(consoleOrigin);
  config = configFor(idp, env);
  consoleServer = startConsoleServer(
    { env, configs: [config], database: memoryAdapter(createConsoleMemoryDatabase()) },
    port,
  );

  // A REAL OpenAI-compatible endpoint, on a real socket. Plain http on loopback:
  // the operator's own box is exactly the case where TLS is often absent, and
  // `canonicalOpenAiBaseUrl` accepts it deliberately.
  vllm = Bun.serve({
    port: 0,
    hostname: "127.0.0.1",
    fetch(incoming) {
      const url = new URL(incoming.url);
      vllmRequests.push(`${incoming.method} ${url.pathname}`);
      if (url.pathname === "/v1/models") {
        return new Response(
          JSON.stringify({ object: "list", data: [{ id: MODEL_KEY, object: "model" }] }),
          { status: 200, headers: { "content-type": "application/json" } },
        );
      }
      return new Response("not found", { status: 404 });
    },
  });
  vllmBaseUrl = `http://127.0.0.1:${vllm.port}/v1`;

  signedInCookie = await signIn();
});

afterAll(() => {
  setConsoleApiDependenciesForTests(null);
  vllm?.stop(true);
  consoleServer?.stop();
  idp?.stop();
  untrustFixtureCa();
  restoreDomWhatwgGlobals();
});

async function json(response: Response): Promise<Record<string, unknown>> {
  return (await response.json()) as Record<string, unknown>;
}

/* -------------------------------------------------------------------------- */

describe("the shortcut, driven with a session the console itself minted", () => {
  test("the same request is refused without the cookie and admitted with it", async () => {
    // Two calls, one difference: the cookie. This is the assertion the whole
    // file exists for — the gate is doing the work, not a stub standing in for
    // it.
    install();
    vllmRequests = [];

    const anonymous = await CONNECT(request({ action: "discover", base_url: vllmBaseUrl }, null));
    expect(anonymous.status).toBe(401);
    expect((await json(anonymous))["error"]).toMatchObject({ code: "no_session" });
    // Refused before the BFF contacted anything at all.
    expect(vllmRequests).toEqual([]);
    expect(stub.routes()).toEqual([]);

    const admitted = await CONNECT(
      request({ action: "discover", base_url: vllmBaseUrl }, signedInCookie),
    );
    expect(admitted.status).toBe(200);
    expect(await json(admitted)).toEqual({ base_url: vllmBaseUrl, models: [MODEL_KEY] });
  });

  test("AND A SESSION WITHOUT AN ADMIN GRANT IS REFUSED BEFORE THE PROBE LEAVES", async () => {
    // The same cookie, minted by the same real sign-in — this is not a session
    // problem. `checkSession` admits any verified address inside
    // `allowed_email_domains`, and it reads no `admin_identities` grant, so for
    // this one handler the session gate was the entire authorization: its whole
    // effect is an outbound GET from inside the deployment's network to a host
    // named in the request body, and no Moira call was ever made to be refused.
    // Moira answering 403 to the grant read is what has to stop it, and it must
    // stop it BEFORE the socket opens.
    install(
      moiraHandlers({
        [PROVIDER_LIST]: () => ({ status: 403, body: errorEnvelope("admin_grant_required") }),
      }),
    );
    vllmRequests = [];

    const response = await CONNECT(
      request({ action: "discover", base_url: vllmBaseUrl }, signedInCookie),
    );
    expect(response.status).toBe(403);
    expect(vllmRequests, "the BFF probed the named host for a caller with no grant").toEqual([]);
  });

  test("DISCOVERY GOES OUT FROM THE BFF, and the browser never touches the endpoint", async () => {
    install();
    vllmRequests = [];
    const response = await CONNECT(
      request({ action: "discover", base_url: `http://127.0.0.1:${vllm.port}` }, signedInCookie),
    );
    expect(response.status).toBe(200);
    // The version segment was appended, and the probe reached the real server.
    expect(vllmRequests).toEqual(["GET /v1/models"]);
    expect((await json(response))["base_url"]).toBe(vllmBaseUrl);
    // Nothing was WRITTEN by a probe. The single Moira call is the admin-grant
    // read that authorizes it — a list, and it comes first.
    expect(stub.routes()).toEqual([PROVIDER_LIST]);
  });

  test("the whole chain runs in the seed script's order and ends routable", async () => {
    install();
    const response = await CONNECT(
      request(
        { action: "connect", base_url: vllmBaseUrl, model_keys: [MODEL_KEY] },
        signedInCookie,
      ),
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

    // The credential row exists even though the endpoint is keyless — the 404
    // `credential_not_found` in `scripts/seed-local.sh`'s header.
    const secret = (stub.bodyOf(CREDENTIAL_CREATE) as Record<string, unknown>)["secret"];
    expect(Object.keys(secret as object)).toEqual(["api_key"]);

    // The policy binds to the SEEDED route, and no route was created.
    const policy = stub.bodyOf(POLICY_CREATE) as Record<string, unknown>;
    expect(policy["route_id"]).toBe(ROUTE_ID);
    expect(stub.routes()).not.toContain("POST /api/v1/admin/routes");

    // Every admin call carried a credential.
    for (const recorded of stub.requests) {
      expect(
        recorded.headers["X-Moira-System-Key"] ?? recorded.headers["Authorization"],
        `${recorded.route} went out unauthenticated`,
      ).toBeString();
    }

    const body = await json(response);
    expect((body["trace"] as Array<{ step: string }>).map((entry) => entry.step)).toEqual([
      "provider",
      "provider_model",
      "provider_credential",
      "provider_enable",
      "routing_policy",
    ]);
  });

  test("a second run creates nothing — reuse-first survives a real repeat", async () => {
    install(
      moiraHandlers({
        [PROVIDER_LIST]: () => ({
          status: 200,
          body: page([
            stamped(PROVIDER_ID, {
              provider_type: "open_ai_compatible",
              display_name: "Local endpoint",
              base_url: vllmBaseUrl,
            }),
          ]),
        }),
        [MODEL_LIST]: () => ({
          status: 200,
          body: page([
            stamped(MODEL_ID, {
              provider_id: PROVIDER_ID,
              model_key: MODEL_KEY,
              capabilities: { text: true, streaming: true, tools: true },
            }),
          ]),
        }),
        [CREDENTIAL_LIST]: () => ({
          status: 200,
          body: page([
            stamped(CREDENTIAL_ID, {
              provider_id: PROVIDER_ID,
              credential_type: "api_key",
              scope: { type: "global" },
              secret_fingerprint: "sha256:fingerprint-that-must-not-cross",
              masked_secret: "sk-****mask",
              priority: 0,
            }),
          ]),
        }),
        [POLICY_LIST]: () => ({
          status: 200,
          body: page([
            stamped(POLICY_ID, {
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
            }),
          ]),
        }),
      }),
    );

    const response = await CONNECT(
      request(
        { action: "connect", base_url: vllmBaseUrl, model_keys: [MODEL_KEY] },
        signedInCookie,
      ),
    );
    expect(response.status).toBe(201);
    expect(stub.routes()).toEqual([
      PROVIDER_LIST,
      MODEL_LIST,
      CREDENTIAL_LIST,
      ROUTE_LIST,
      POLICY_LIST,
    ]);
    for (const write of [PROVIDER_CREATE, MODEL_CREATE, CREDENTIAL_CREATE, POLICY_CREATE]) {
      expect(stub.routes(), `${write} ran a second time`).not.toContain(write);
    }
  });

  test("the screen the operator lands on afterwards carries no secret material", async () => {
    install(
      moiraHandlers({
        [PROVIDER_LIST]: () => ({
          status: 200,
          body: page([
            stamped(PROVIDER_ID, {
              provider_type: "open_ai_compatible",
              display_name: "Local endpoint",
              base_url: vllmBaseUrl,
            }),
          ]),
        }),
        [CREDENTIAL_LIST]: () => ({
          status: 200,
          body: page([
            stamped(CREDENTIAL_ID, {
              provider_id: PROVIDER_ID,
              credential_type: "api_key",
              scope: { type: "global" },
              secret_fingerprint: "sha256:fingerprint-that-must-not-cross",
              masked_secret: "sk-****mask",
              priority: 0,
            }),
          ]),
        }),
      }),
    );

    const response = await PROVIDERS_GET(
      new Request(`${consoleServer.origin}/api/llm/providers`, {
        headers: { cookie: signedInCookie },
      }),
    );
    expect(response.status).toBe(200);
    const serialised = JSON.stringify(await json(response));
    expect(serialised).not.toContain("sha256:fingerprint-that-must-not-cross");
    expect(serialised).not.toContain("sk-****mask");
    expect(serialised).toContain(CREDENTIAL_ID);
  });
});
