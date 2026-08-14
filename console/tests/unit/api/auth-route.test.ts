// The Better Auth mount point's own behaviour — issue #152's third criterion.
//
// ============================================================================
// WHAT WENT WRONG, VERBATIM
// ============================================================================
//
//     POST /api/auth/sign-in/oauth2 500
//     TypeError: fetch failed
//       [cause]: AggregateError: ... code: 'ECONNREFUSED'
//
// The console was serving a configuration that had been superseded in Moira and
// dialling an endpoint that no longer existed. Nothing in that output names
// configuration: it reads as "the identity provider is down", and the operator
// went looking at an IdP that was fine.
//
// The TTL and `invalidateAuthConfig` are what stop the console getting here. This
// file is about what it says when it does — which it still can, because an
// operator can also simply mistype an endpoint.
//
// ============================================================================
// THE PROVIDER IS REALLY UNREACHABLE, NOT A STUB THAT THROWS
// ============================================================================
//
// A hand-thrown `new TypeError("fetch failed")` would test the predicate against
// the test's own idea of what Node produces. So the fixture points a REAL
// `genericOAuth` provider at a closed port on the loopback interface and lets
// Better Auth dial it: the error that arrives at the route is the one undici
// actually constructs, `AggregateError` wrapper and all.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";

import { handleAuthRequest, isProviderUnreachable } from "@/app/api/auth/[...all]/route";
import type { ResolvedAuthConfig } from "@/lib/auth-config";
import { createConsoleAuth } from "@/lib/auth";
import { readConsoleEnv, type ConsoleEnv, type EnvSource } from "@/lib/env";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { restoreDomWhatwgGlobals, useNativeWhatwgGlobals } from "../../support/native-globals";

// A SERVER suite, so it runs on Bun's own WHATWG globals. happy-dom's `fetch` is
// a `node:http` wrapper that reports a refused connection as a `DOMException`
// rather than as undici's `TypeError: fetch failed` — a different error shape
// from the one production throws, which would make this file test the harness.
beforeAll(() => {
  useNativeWhatwgGlobals();
});

afterAll(() => {
  restoreDomWhatwgGlobals();
});

const CONSOLE_ORIGIN = "https://console.example.com";

/**
 * Port 1 on the loopback interface.
 *
 * Privileged, unbound, and refused immediately rather than left to time out —
 * so this test costs a syscall rather than a connect timeout.
 */
const CLOSED_ENDPOINT = "http://127.0.0.1:1";

const BASE_ENV: EnvSource = {
  NODE_ENV: "test",
  MOIRA_API_URL: "https://moira.test",
  CONSOLE_PUBLIC_ORIGIN: CONSOLE_ORIGIN,
  MOIRA_ADMIN_API_AUDIENCE: "moira-admin-audience",
  BETTER_AUTH_SECRET: "a-secret-that-is-at-least-32-characters",
  CONSOLE_SECRET_ENCRYPTION_KEY: Buffer.alloc(32, 5).toString("base64"),
};

const env: ConsoleEnv = readConsoleEnv(BASE_ENV);

/** One provider whose every endpoint points at something nothing answers on. */
const UNREACHABLE_CONFIG: ResolvedAuthConfig = {
  providerId: "moira-console-idp",
  consoleIssuer: CONSOLE_ORIGIN,
  trustedJwtIssuerId: "22222222-2222-4222-8222-222222222222",
  method: "generic_oidc",
  moiraProviderId: "11111111-1111-4111-8111-111111111111",
  moiraProviderVersion: 4,
  issuer: CLOSED_ENDPOINT,
  discoveryUrl: `${CLOSED_ENDPOINT}/.well-known/openid-configuration`,
  authorizationUrl: null,
  tokenUrl: null,
  userInfoUrl: null,
  clientId: "console.apps.idp.test",
  clientSecret: "the-client-secret-fixture",
  scopes: ["openid", "email"],
  allowedEmailDomains: ["example.com"],
};

function signInRequest(): Request {
  return new Request(`${CONSOLE_ORIGIN}/api/auth/sign-in/oauth2`, {
    method: "POST",
    headers: { "content-type": "application/json", origin: CONSOLE_ORIGIN },
    body: JSON.stringify({ providerId: UNREACHABLE_CONFIG.providerId, callbackURL: "/" }),
  });
}

/* -------------------------------------------------------------------------- */
/* The end-to-end shape                                                       */
/* -------------------------------------------------------------------------- */

describe("a configuration the console cannot reach is named as such", () => {
  test("sign-in against a dead endpoint is a keyed 503, not a bare fetch error", async () => {
    const auth = createConsoleAuth({ env, configs: [UNREACHABLE_CONFIG] });

    const response = await handleAuthRequest(signInRequest(), async () => ({
      ok: true,
      auth,
      configs: [UNREACHABLE_CONFIG],
      problems: [],
      stale: false,
    }));

    expect(response.status).toBe(503);
    const body = (await response.json()) as { error: { code: string; message_key: string } };
    expect(body.error.code).toBe("auth_provider_unreachable");
    expect(body.error.message_key).toBe(CONSOLE_MESSAGE_KEYS.auth_provider_unreachable);
  });

  test("nothing from the thrown error reaches the body", async () => {
    // The same rule `toTransportError` states and `SignInPanel` restates: a
    // thrown fetch error can carry a URL with credentials in it. So the response
    // carries a key and a code, and no message, no cause, no endpoint, no errno.
    const auth = createConsoleAuth({ env, configs: [UNREACHABLE_CONFIG] });
    const response = await handleAuthRequest(signInRequest(), async () => ({
      ok: true,
      auth,
      configs: [UNREACHABLE_CONFIG],
      problems: [],
      stale: false,
    }));

    const text = await response.text();
    expect(text).not.toContain("127.0.0.1");
    expect(text).not.toContain("ECONNREFUSED");
    expect(text).not.toContain("fetch failed");
    expect(text).not.toContain(UNREACHABLE_CONFIG.clientSecret);
    expect(response.headers.get("cache-control")).toBe("no-store");
  });
});

/* -------------------------------------------------------------------------- */
/* The predicate, and the half of it that must keep saying no                 */
/* -------------------------------------------------------------------------- */

describe("isProviderUnreachable", () => {
  test("recognises undici's wrapper, the errno, and an AggregateError of them", () => {
    expect(isProviderUnreachable(new TypeError("fetch failed"))).toBe(true);
    expect(isProviderUnreachable(Object.assign(new Error("x"), { code: "ECONNREFUSED" }))).toBe(
      true,
    );
    // The shape #152's reproduction actually showed: one hostname, several
    // addresses, every one of them refused.
    const aggregate = new AggregateError([
      Object.assign(new Error("connect ECONNREFUSED"), { code: "ECONNREFUSED" }),
    ]);
    expect(isProviderUnreachable(new Error("outer", { cause: aggregate }))).toBe(true);
  });

  test("an ordinary console bug is NOT reported as an unreachable provider", () => {
    // THE MUTATION THIS EXISTS FOR: widen the predicate to `error instanceof
    // Error` and every genuine fault inside Better Auth's handler turns into a
    // soothing "check your provider's endpoints" — #152's defect with the arrow
    // reversed, an operator sent to inspect configuration that is fine.
    expect(isProviderUnreachable(new Error("cannot read properties of undefined"))).toBe(false);
    expect(isProviderUnreachable(new TypeError("x is not a function"))).toBe(false);
    expect(isProviderUnreachable({ code: "ENOENT" })).toBe(false);
    expect(isProviderUnreachable(null)).toBe(false);
    expect(isProviderUnreachable("ECONNREFUSED")).toBe(false);
  });

  test("a cause chain that never terminates does not hang the handler", () => {
    // A self-referential `cause` is constructible, and an unbounded walk over one
    // never returns — which would convert a refused connection into a hung
    // request, on the endpoint whose whole problem is that it does not answer.
    const looping: { cause?: unknown } = {};
    looping.cause = looping;
    expect(isProviderUnreachable(looping)).toBe(false);
  });
});
