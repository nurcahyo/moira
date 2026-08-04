// The session -> Moira-credential bridge, and the two rules it exists to keep.

import { describe, expect, test } from "bun:test";

import type { ConsoleEnv } from "@/lib/env";
import { readConsoleEnv } from "@/lib/env";
import {
  assertAdminPlanePath,
  checkSession,
  mintMoiraToken,
  moiraClientForSession,
  moiraClientForSetup,
  MoiraPlaneViolationError,
} from "@/lib/moira-session";

import { createMoiraStub } from "../../support/moira-stub";

function envWith(overrides: Record<string, string> = {}): ConsoleEnv {
  return readConsoleEnv({
    NODE_ENV: "test",
    MOIRA_API_URL: "https://moira.test",
    CONSOLE_PUBLIC_ORIGIN: "https://console.test",
    MOIRA_ADMIN_API_AUDIENCE: "moira-admin-api",
    BETTER_AUTH_SECRET: "a-secret-that-is-at-least-32-characters",
    CONSOLE_SECRET_ENCRYPTION_KEY: Buffer.alloc(32, 5).toString("base64"),
    ...overrides,
  });
}

/**
 * The authenticating configuration, reduced to what `checkSession` reads.
 *
 * `consoleIssuer` joined it when the setup window's claim step needed to check
 * a caller-supplied namespace against the provider the session was actually
 * established through (issue #71): the verdict now carries the issuer of the
 * configuration that produced it, so it has to be supplied here.
 */
const ALLOW = {
  allowedEmailDomains: ["example.com"],
  consoleIssuer: "https://console.example",
};

describe("checkSession", () => {
  test("accepts a verified, allow-listed identity with a subject", () => {
    const result = checkSession(
      { email: "operator@example.com", emailVerified: true, idpSubject: "sub-1" },
      ALLOW,
    );
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.identity.idpSubject).toBe("sub-1");
  });

  test("no session", () => {
    expect(checkSession(null, ALLOW).ok).toBe(false);
    expect(checkSession(undefined, ALLOW).ok).toBe(false);
  });

  test("an unverified address is refused — Moira refuses the claim anyway", () => {
    const result = checkSession(
      { email: "operator@example.com", emailVerified: false, idpSubject: "sub-1" },
      ALLOW,
    );
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.rejection).toBe("email_not_verified");
  });

  test("an outside domain is refused even though the IdP authenticated them", () => {
    // Without this the stranger holds no Moira authority but IS inside the
    // console shell, and every error the UI renders is one more thing they see.
    const result = checkSession(
      { email: "stranger@elsewhere.test", emailVerified: true, idpSubject: "sub-2" },
      ALLOW,
    );
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.rejection).toBe("email_domain_not_allowed");
  });

  test("a session with no IdP subject is refused", () => {
    const result = checkSession({ email: "operator@example.com", emailVerified: true }, ALLOW);
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.rejection).toBe("idp_subject_missing");
  });

  test("every rejection carries an i18n key, never English prose", () => {
    for (const session of [
      null,
      { email: "operator@example.com", emailVerified: false, idpSubject: "s" },
      { email: "stranger@elsewhere.test", emailVerified: true, idpSubject: "s" },
      { email: "operator@example.com", emailVerified: true },
    ]) {
      const result = checkSession(session, ALLOW);
      expect(result.ok).toBe(false);
      if (result.ok) continue;
      expect(result.messageKey).toStartWith("console.error.");
    }
  });
});

describe("the admin-plane restriction (plan 07 decision D2)", () => {
  test("admin paths pass", () => {
    expect(() => assertAdminPlanePath("/api/v1/admin/auth/providers")).not.toThrow();
  });

  test("a public-plane path is refused", () => {
    // `authenticate_caller` does NOT apply the admin_identities grant, so the
    // same token confers no authority there. The console makes zero non-admin
    // calls and this is what keeps it that way.
    expect(() => assertAdminPlanePath("/api/v1/executions")).toThrow(MoiraPlaneViolationError);
  });

  test("a path that merely contains the prefix does not pass", () => {
    expect(() => assertAdminPlanePath("/public/api/v1/admin/x")).toThrow(MoiraPlaneViolationError);
  });
});

describe("client construction", () => {
  test("the session client sends the operator's bearer token, NOT the system key", async () => {
    // MoiraClient prefers the system key when both are present, so passing it
    // here would authenticate every admin call as the bootstrap key and stop
    // the audit trail naming humans.
    const stub = createMoiraStub({
      "GET /api/v1/admin/auth/providers": () => ({
        status: 200,
        body: { data: [], pagination: { has_more: false, next_cursor: null } },
      }),
    });
    const client = moiraClientForSession(
      envWith({ MOIRA_SYSTEM_KEY: "sk_bootstrap" }),
      { api: { getToken: async () => ({ token: "minted.jwt.value" }) } },
      new Headers(),
      { fetch: stub.fetch },
    );
    await client.listAuthProviders();

    const request = stub.requests[0];
    expect(request?.headers["Authorization"]).toBe("Bearer minted.jwt.value");
    expect(request?.headers["X-Moira-System-Key"]).toBeUndefined();
  });

  test("the setup client refuses to exist without the bootstrap key", () => {
    expect(() => moiraClientForSetup(envWith())).toThrow(/MOIRA_SYSTEM_KEY is not set/);
  });

  test("the setup client's error explains why a bearer token will not do", () => {
    // `POST /api/v1/admin/setup/claim` declares systemKeyAuth and NOTHING else.
    try {
      moiraClientForSetup(envWith());
      expect.unreachable();
    } catch (error) {
      expect((error as Error).message).toContain("setup_claim_credential_required");
    }
  });

  test("the setup client is constructible when the key is present", () => {
    expect(() => moiraClientForSetup(envWith({ MOIRA_SYSTEM_KEY: "sk_bootstrap" }))).not.toThrow();
  });
});

describe("mintMoiraToken", () => {
  test("returns the plugin's token", async () => {
    const token = await mintMoiraToken(
      { api: { getToken: async () => ({ token: "a.b.c" }) } },
      new Headers(),
    );
    expect(token).toBe("a.b.c");
  });

  test("an empty token is an error, not an empty Authorization header", async () => {
    await expect(
      mintMoiraToken({ api: { getToken: async () => ({ token: "" }) } }, new Headers()),
    ).rejects.toThrow(/no token/);
  });

  test("the caller's headers are forwarded — the session cookie lives there", async () => {
    let seen: Headers | undefined;
    const headers = new Headers({ cookie: "session_token=abc" });
    await mintMoiraToken(
      {
        api: {
          getToken: async (options) => {
            seen = options.headers;
            return { token: "t" };
          },
        },
      },
      headers,
    );
    expect(seen?.get("cookie")).toBe("session_token=abc");
  });
});
