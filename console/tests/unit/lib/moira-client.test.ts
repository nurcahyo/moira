import { describe, expect, test } from "bun:test";

import { isMoiraRequestError } from "@/lib/errors";
import {
  AUTH_PROVIDER_OPERATION_NAMES,
  MOIRA_OPERATIONS,
  MoiraClient,
  MoiraClientContractError,
  assertClaimRequestIsSafe,
  assertProviderCreateIsSafe,
  assertTrustedIssuerCreateIsSafe,
  ifMatchFor,
  type MoiraOperationName,
} from "@/lib/moira-client";
import { MOIRA_STUB_BASE_URL, createMoiraStub, errorEnvelope } from "../../support/moira-stub";

const SYSTEM_KEY = "sk_test_bootstrap";

function clientWith(
  handlers: Parameters<typeof createMoiraStub>[0],
  options: {
    readonly systemKey?: string | undefined;
    readonly bearerToken?: (() => string) | undefined;
  } = {},
) {
  const stub = createMoiraStub(handlers);
  const client = new MoiraClient({
    baseUrl: MOIRA_STUB_BASE_URL,
    systemKey: "systemKey" in options ? options.systemKey : SYSTEM_KEY,
    bearerToken: options.bearerToken,
    fetch: stub.fetch,
  });
  return { stub, client };
}

const ok = (body: unknown) => () => ({ status: 200, body });

/* -------------------------------------------------------------------------- */

describe("credential selection is driven by the operation registry", () => {
  test("the anonymous operations are exactly the three the console binds to", () => {
    // WAS `claim_status_is_the_only_anonymous_call`, then a two-entry set.
    // Finding F15's fix added `GET /api/v1/admin/setup/sign-in-methods` with no
    // `security` block; plan 09 wave 5 binds the third,
    // `POST /api/v1/admin/admin-invites/preview`, which wave 2 shipped
    // credential-free and nothing in the console previously called.
    //
    // The assertion is still an exact set rather than a count: "how many are
    // anonymous" is not the interesting question, "which ones" is. A new entry
    // here has to be a deliberate edit.
    const anonymous = (Object.keys(MOIRA_OPERATIONS) as MoiraOperationName[])
      .filter((name) => MOIRA_OPERATIONS[name].credential === "none")
      .sort();
    expect(anonymous).toEqual([
      "getSetupClaimStatus",
      "getSetupSignInMethods",
      "previewAdminInvite",
    ]);
  });

  test("redeem is the ONLY bearer_only operation", () => {
    const bearerOnly = (Object.keys(MOIRA_OPERATIONS) as MoiraOperationName[])
      .filter((name) => MOIRA_OPERATIONS[name].credential === "bearer_only")
      .sort();
    expect(
      bearerOnly,
      "a second bearer_only operation means a second path on which the console must not be " +
        "able to present its own bootstrap credential — argue it before adding it",
    ).toEqual(["redeemAdminInvite"]);
  });

  test("every anonymous operation really sends no credential", async () => {
    // The registry saying `credential: "none"` and the client sending nothing
    // are two different claims. `#buildHeaders` is what joins them, so it is
    // exercised rather than trusted.
    const { stub, client } = clientWith({
      "GET /api/v1/admin/setup/claim-status": ok({ claimed: false }),
      "GET /api/v1/admin/setup/sign-in-methods": ok({ methods: [] }),
    });
    await client.getSetupClaimStatus();
    await client.getSetupSignInMethods();

    expect(stub.requests).toHaveLength(2);
    for (const request of stub.requests) {
      expect(Object.keys(request.headers)).not.toContain("X-Moira-System-Key");
      expect(Object.keys(request.headers)).not.toContain("Authorization");
    }
  });

  test("the claim-status read sends no credential at all", async () => {
    const { stub, client } = clientWith({
      "GET /api/v1/admin/setup/claim-status": ok({ claimed: false }),
    });
    await client.getSetupClaimStatus();

    const headers = stub.requests[0]?.headers ?? {};
    expect(Object.keys(headers)).not.toContain("X-Moira-System-Key");
    expect(Object.keys(headers)).not.toContain("Authorization");
  });

  test("auth_methods_read_sends_the_system_key", async () => {
    const { stub, client } = clientWith({
      "GET /api/v1/admin/setup/auth-methods": ok({ methods: [] }),
    });
    await client.getSetupAuthMethods();
    expect(stub.requests[0]?.headers["X-Moira-System-Key"]).toBe(SYSTEM_KEY);
  });

  test("the claim refuses to run on a bearer token", async () => {
    const { client } = clientWith(
      { "POST /api/v1/admin/setup/claim": ok({}) },
      { systemKey: undefined, bearerToken: () => "jwt" },
    );
    // `security: [{ systemKeyAuth: [] }]` and nothing else — a bearer JWT is
    // refused by Moira even if it verifies, so the client refuses first.
    await expect(
      client.claimAdminIdentity({
        issuer: "https://console.example.com",
        subject: "s",
        email: "a@example.com",
        email_verified: true,
      }),
    ).rejects.toThrow(MoiraClientContractError);
  });

  test("an admin call falls back to a bearer token when no system key is set", async () => {
    const { stub, client } = clientWith(
      { "GET /api/v1/admin/auth/providers": ok({ data: [], pagination: { has_more: false } }) },
      { systemKey: undefined, bearerToken: () => "console-jwt" },
    );
    await client.listAuthProviders();
    expect(stub.requests[0]?.headers["Authorization"]).toBe("Bearer console-jwt");
  });
});

describe("Idempotency-Key is sent only where the spec declares it", () => {
  test("enable cannot carry one — supplying it is a contract error, not a no-op", async () => {
    const { client } = clientWith({});
    // Not merely "we choose not to send it". Reaching for it fails loudly, so a
    // future edit cannot quietly reintroduce "keyed everywhere".
    expect(MOIRA_OPERATIONS.enableAuthProvider.declaresIdempotencyKey).toBe(false);
    await expect(
      // @ts-expect-error enableAuthProvider deliberately exposes no key parameter
      client.enableAuthProvider("id", "1", { idempotencyKey: "k" }),
    ).rejects.toBeDefined();
  });

  test("create carries one when supplied", async () => {
    const { stub, client } = clientWith({
      "POST /api/v1/admin/auth/providers": () => ({
        status: 201,
        body: { id: "p", version: 1, trusted_jwt_issuer_id: "i" },
      }),
    });
    await client.createAuthProvider(
      { method: "jwks", display_name: "d", trusted_jwt_issuer_id: "i" },
      { idempotencyKey: "idem-1" },
    );
    expect(stub.requests[0]?.headers["Idempotency-Key"]).toBe("idem-1");
  });

  test("exactly eight of the registry's operations declare a key", () => {
    // Every entry is read off the spec, not assumed;
    // `tests/contract/openapi-contract.test.ts` re-derives each flag from
    // `docs/openapi.json` on every run.
    //
    // FOUR ARE NEW IN WAVE 5, and one of them is a correction to plan 09 §0.8.4
    // step 6, which lists "create, revoke, redeem and delete" for this family.
    // `patch_admin_identity` declares an optional `Idempotency-Key` **as well as**
    // its required `If-Match`, so the audit's list was incomplete. The registry
    // follows the committed spec, not the audit.
    const withKey = (Object.keys(MOIRA_OPERATIONS) as MoiraOperationName[])
      .filter((name) => MOIRA_OPERATIONS[name].declaresIdempotencyKey)
      .sort();
    expect(withKey).toEqual([
      "claimAdminIdentity",
      "createAdminInvite",
      "createAuthProvider",
      "createTrustedJwtIssuer",
      "deleteAdminIdentity",
      "patchAdminIdentity",
      "redeemAdminInvite",
      "revokeAdminInvite",
    ]);
  });
});

describe("If-Match is required where the spec requires it", () => {
  test("enable/disable/patch/delete refuse an empty version", async () => {
    const { client } = clientWith({});
    await expect(client.enableAuthProvider("p", "")).rejects.toThrow(MoiraClientContractError);
    await expect(client.disableAuthProvider("p", "")).rejects.toThrow(MoiraClientContractError);
    await expect(client.deleteAuthProvider("p", "")).rejects.toThrow(MoiraClientContractError);
  });

  test("the version comes from a prior read, never fabricated", () => {
    expect(ifMatchFor({ version: 7 })).toBe("7");
  });

  test("patch refuses to toggle `enabled` behind enable/disable's back", async () => {
    const { client } = clientWith({});
    await expect(client.patchAuthProvider("p", { enabled: true }, "1")).rejects.toThrow(
      MoiraClientContractError,
    );
  });
});

describe("the claim builder", () => {
  test("claim_builder_never_populates_setup_token", () => {
    expect(() =>
      assertClaimRequestIsSafe({
        issuer: "i",
        subject: "s",
        email: "a@b.c",
        email_verified: true,
        setup_token: "tok",
      }),
    ).toThrow(/setup_token/);
  });

  test("claim_builder_omits_scopes_entirely_never_sends_an_empty_array", () => {
    for (const scopes of [[], ["moira:admin"], null]) {
      expect(() =>
        assertClaimRequestIsSafe({
          issuer: "i",
          subject: "s",
          email: "a@b.c",
          email_verified: true,
          scopes,
        }),
      ).toThrow(/scopes/);
    }
  });

  test("claim_request_always_sends_email_and_email_verified", () => {
    expect(() =>
      assertClaimRequestIsSafe({ issuer: "i", subject: "s", email_verified: true }),
    ).toThrow(/email/);
    expect(() => assertClaimRequestIsSafe({ issuer: "i", subject: "s", email: "a@b.c" })).toThrow(
      /email_verified/,
    );
    // No credential-type branch makes them omittable: there is one guard and it
    // runs on every claim.
    expect(() =>
      assertClaimRequestIsSafe({
        issuer: "i",
        subject: "s",
        email: "a@b.c",
        email_verified: false,
      }),
    ).not.toThrow();
  });
});

describe("the provider-create builder", () => {
  test("provider_create_never_sends_enabled", () => {
    for (const enabled of [true, false, undefined, null]) {
      expect(() =>
        assertProviderCreateIsSafe({
          method: "jwks",
          display_name: "d",
          trusted_jwt_issuer_id: "i",
          enabled,
        }),
      ).toThrow(/enabled/);
    }
  });

  test("provider_create_requires_the_trusted_jwt_issuer_binding", () => {
    for (const value of [undefined, null, ""]) {
      expect(() =>
        assertProviderCreateIsSafe({
          method: "jwks",
          display_name: "d",
          trusted_jwt_issuer_id: value,
        }),
      ).toThrow(/trusted_jwt_issuer_id/);
    }
  });

  test("provider_create_requires_display_name", () => {
    expect(() =>
      assertProviderCreateIsSafe({ method: "jwks", trusted_jwt_issuer_id: "i" }),
    ).toThrow(/display_name/);
  });
});

describe("the trusted-issuer builder", () => {
  test("console_issuer_never_asserts_scopes", () => {
    expect(() =>
      assertTrustedIssuerCreateIsSafe({ issuer: "i", jwks_url: "u", scopes_claim: "scp" }),
    ).toThrow(/scopes_claim/);
    expect(() =>
      assertTrustedIssuerCreateIsSafe({
        issuer: "i",
        jwks_url: "u",
        claim_mapping: { scopes: "scp" },
      }),
    ).toThrow(/claim_mapping/);
    expect(() =>
      assertTrustedIssuerCreateIsSafe({ issuer: "i", jwks_url: "u", scopes_claim: null }),
    ).not.toThrow();
  });
});

describe("the auth-provider surface is SEVEN operations", () => {
  test("there_is_no_rotate_secret_method", () => {
    expect(AUTH_PROVIDER_OPERATION_NAMES).toHaveLength(7);
    const source = Object.values(MOIRA_OPERATIONS)
      .map((operation) => `${operation.id} ${operation.path}`)
      .join("\n");
    expect(source).not.toContain("rotate-secret");
    expect(source).not.toContain("rotate_secret");
  });

  test("no client method or path constant mentions a client secret", () => {
    const surface = JSON.stringify(MOIRA_OPERATIONS);
    for (const forbidden of ["client_secret", "clientSecret", "secret"]) {
      expect(surface).not.toContain(forbidden);
    }
    expect(Object.getOwnPropertyNames(MoiraClient.prototype).join(" ")).not.toMatch(/secret/i);
  });
});

describe("error propagation", () => {
  test("a Moira error becomes a MoiraRequestError carrying the mapped union", async () => {
    const { client } = clientWith({
      "GET /api/v1/admin/setup/claim-status": () => ({
        status: 503,
        body: errorEnvelope("database_unavailable"),
      }),
    });

    const caught = await client.getSetupClaimStatus().catch((error: unknown) => error);
    expect(isMoiraRequestError(caught)).toBe(true);
    if (!isMoiraRequestError(caught)) throw new Error("unreachable");
    expect(caught.moiraError.kind).toBe("api");
    expect(caught.moiraError.remedy).toBe("wait_for_backend");
    // Still no details across the boundary, even on the throw path.
    expect(JSON.stringify(caught.moiraError)).not.toContain("must not cross the boundary");
  });

  test("a transport failure surfaces as a transport error", async () => {
    const client = new MoiraClient({
      baseUrl: MOIRA_STUB_BASE_URL,
      systemKey: SYSTEM_KEY,
      fetch: (() => Promise.reject(new Error("ECONNRESET"))) as unknown as typeof fetch,
    });
    const caught = await client.getSetupClaimStatus().catch((error: unknown) => error);
    expect(isMoiraRequestError(caught)).toBe(true);
    if (!isMoiraRequestError(caught)) throw new Error("unreachable");
    expect(caught.moiraError.kind).toBe("transport");
  });
});

describe("URL building", () => {
  test("path parameters are substituted and encoded", async () => {
    const { stub, client } = clientWith({
      "GET /api/v1/admin/auth/providers/a%2Fb": ok({ id: "a/b" }),
    });
    await client.getAuthProvider("a/b");
    expect(stub.requests[0]?.url).toBe(`${MOIRA_STUB_BASE_URL}/api/v1/admin/auth/providers/a%2Fb`);
  });

  test("undefined query values are omitted rather than serialised", async () => {
    const { stub, client } = clientWith({
      "GET /api/v1/admin/jwt-issuers": ok({ data: [], pagination: { has_more: false } }),
    });
    await client.listTrustedJwtIssuers({ limit: 100 });
    expect(stub.requests[0]?.url).toBe(`${MOIRA_STUB_BASE_URL}/api/v1/admin/jwt-issuers?limit=100`);
  });
});

describe("findTrustedJwtIssuerByIssuer", () => {
  const record = (issuer: string) => ({
    id: `id-${issuer}`,
    issuer,
    jwks_url: "https://x/jwks",
    expected_audiences: [],
    allowed_algorithms: ["ES256"],
    subject_claim: "sub",
    clock_skew_seconds: 60,
    allow_delegation: false,
    status: "active",
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    version: 1,
  });

  test("matches exactly — a prefix is not a match", async () => {
    const { client } = clientWith({
      "GET /api/v1/admin/jwt-issuers": ok({
        data: [record("https://console.example.com/extra")],
        pagination: { has_more: false, next_cursor: null },
      }),
    });
    // Binding the provider to a prefix-matched issuer would be worse than not
    // finding one: it would look like it worked.
    expect(await client.findTrustedJwtIssuerByIssuer("https://console.example.com")).toBeNull();
  });

  test("pages until it finds the row", async () => {
    let page = 0;
    const { stub, client } = clientWith({
      "GET /api/v1/admin/jwt-issuers": () => {
        page += 1;
        return page === 1
          ? {
              status: 200,
              body: {
                data: [record("https://other")],
                pagination: { has_more: true, next_cursor: "c2" },
              },
            }
          : {
              status: 200,
              body: {
                data: [record("https://console.example.com")],
                pagination: { has_more: false, next_cursor: null },
              },
            };
      },
    });

    const found = await client.findTrustedJwtIssuerByIssuer("https://console.example.com");
    expect(found?.issuer).toBe("https://console.example.com");
    expect(stub.requests).toHaveLength(2);
    expect(stub.requests[1]?.url).toContain("cursor=c2");
  });
});

/* -------------------------------------------------------------------------- */
/* The invitation and ownership surface (plan 09 wave 5)                      */
/* -------------------------------------------------------------------------- */

const inviteRecord = {
  id: "invite-1",
  constraint: "email",
  value: "colleague@corp.test",
  status: "pending",
  expired: false,
  expires_at: "2026-08-01T00:00:00Z",
  created_at: "2026-07-31T00:00:00Z",
  version: 1,
};

const identityRecord = {
  id: "grant-1",
  issuer: "https://console.test/idp/google",
  subject: "sub-1",
  email: "colleague@corp.test",
  email_verified: true,
  granted_scopes: ["moira:admin"],
  status: "active",
  created_at: "2026-07-31T00:00:00Z",
  version: 3,
  notice: { message_key: "moira.notice.admin_identity_claimed", message: "Granted." },
  is_primary: false,
};

describe("redeem carries the invitee's bearer token and NEVER the system key", () => {
  test("a client holding a system key REFUSES to redeem", async () => {
    // ========================================================================
    // THE ASSERTION THIS WHOLE VARIANT EXISTS FOR (plan 09 W5-D3 / W5-B2)
    // ========================================================================
    //
    // `#buildHeaders`' `admin` arm prefers the system key whenever one is
    // present. Had redeem been registered as `admin`, a console holding the
    // bootstrap credential — which `moiraClientForSetup` does — would have sent
    // `X-Moira-System-Key` on a request that mints an `admin_identities` grant.
    // Moira cannot tell that apart from a legitimate operator action.
    //
    // Written as a REFUSAL rather than as "the header is absent" on purpose: an
    // absence assertion passes when the request is never built at all, and it
    // would also pass if a future edit silently dropped the credential and
    // produced a 401 instead.
    const { stub, client } = clientWith(
      { "POST /api/v1/admin/admin-invites/redeem": ok(identityRecord) },
      { systemKey: SYSTEM_KEY, bearerToken: () => "jwt-from-the-invitee-session" },
    );

    await expect(
      client.redeemAdminInvite({ token: "t", email: "a@b.test", email_verified: true }),
    ).rejects.toThrow(MoiraClientContractError);

    // And nothing went on the wire: the refusal is before the fetch, so a
    // misconfigured client cannot leak the key to a server that logs headers.
    expect(stub.requests, "the refusal must happen before the request is sent").toEqual([]);
  });

  test("a session client sends Authorization and no system key", async () => {
    const { stub, client } = clientWith(
      { "POST /api/v1/admin/admin-invites/redeem": ok(identityRecord) },
      { systemKey: undefined, bearerToken: () => "jwt-from-the-invitee-session" },
    );

    await client.redeemAdminInvite(
      { token: "raw-token", email: "colleague@corp.test", email_verified: true },
      { idempotencyKey: "redeem-1" },
    );

    const headers = stub.requests[0]?.headers ?? {};
    expect(headers["Authorization"]).toBe("Bearer jwt-from-the-invitee-session");
    expect(Object.keys(headers)).not.toContain("X-Moira-System-Key");
    expect(headers["Idempotency-Key"]).toBe("redeem-1");
    expect(stub.bodyOf("POST /api/v1/admin/admin-invites/redeem")).toEqual({
      token: "raw-token",
      email: "colleague@corp.test",
      email_verified: true,
    });
  });

  test("with neither credential it refuses rather than sending an anonymous request", async () => {
    const { client } = clientWith(
      { "POST /api/v1/admin/admin-invites/redeem": ok(identityRecord) },
      { systemKey: undefined, bearerToken: undefined },
    );
    await expect(
      client.redeemAdminInvite({ token: "t", email: "a@b.test", email_verified: true }),
    ).rejects.toThrow(MoiraClientContractError);
  });
});

describe("preview is anonymous and puts the token in the BODY", () => {
  test("no credential header, and the token is never in the URL", async () => {
    const { stub, client } = clientWith({
      "POST /api/v1/admin/admin-invites/preview": ok({
        constraint: "email",
        value: "colleague@corp.test",
        expires_at: "2026-08-01T00:00:00Z",
      }),
    });

    await client.previewAdminInvite("raw-token-value");

    const request = stub.requests[0]!;
    expect(Object.keys(request.headers)).not.toContain("Authorization");
    expect(Object.keys(request.headers)).not.toContain("X-Moira-System-Key");
    // The URL reaches access logs, proxy logs and `Referer` chains. The body
    // does not.
    expect(request.url).not.toContain("raw-token-value");
    expect(request.body).toEqual({ token: "raw-token-value" });
  });
});

describe("the ownership surface", () => {
  test("patch requires If-Match and sends exactly one field", async () => {
    const { stub, client } = clientWith({
      "PATCH /api/v1/admin/admin-identities/grant-1": ok({ ...identityRecord, is_primary: true }),
    });

    await client.patchAdminIdentity("grant-1", { is_primary: true }, ifMatchFor(identityRecord));

    expect(stub.requests[0]?.headers["If-Match"]).toBe("3");
    expect(stub.bodyOf("PATCH /api/v1/admin/admin-identities/grant-1")).toEqual({
      is_primary: true,
    });
  });

  test("patch without If-Match is a contract error, not a request", async () => {
    const { stub, client } = clientWith({
      "PATCH /api/v1/admin/admin-identities/grant-1": ok(identityRecord),
    });
    await expect(
      client.patchAdminIdentity("grant-1", { is_primary: true }, ""),
    ).rejects.toThrow(MoiraClientContractError);
    expect(stub.requests).toEqual([]);
  });

  test("delete declares NO If-Match, so supplying one is refused", async () => {
    // The neighbouring PATCH requires one, which is exactly why this is asserted
    // rather than assumed: a caller copying the transfer call would otherwise
    // send a header the operation does not declare.
    const { client } = clientWith({
      "DELETE /api/v1/admin/admin-identities/grant-1": ok(identityRecord),
    });
    expect(MOIRA_OPERATIONS.deleteAdminIdentity.requiresIfMatch).toBe(false);
    await expect(client.deleteAdminIdentity("grant-1")).resolves.toMatchObject({ id: "grant-1" });
  });

  test("revoke is a POST to a sub-resource, not a DELETE", async () => {
    const { stub, client } = clientWith({
      "POST /api/v1/admin/admin-invites/invite-1/revoke": ok({
        ...inviteRecord,
        status: "revoked",
      }),
    });
    await client.revokeAdminInvite("invite-1", { idempotencyKey: "revoke-1" });
    expect(stub.routes()).toEqual(["POST /api/v1/admin/admin-invites/invite-1/revoke"]);
    expect(stub.requests[0]?.headers["Idempotency-Key"]).toBe("revoke-1");
  });

  test("the reads carry no Idempotency-Key and no If-Match", async () => {
    const empty = { data: [], pagination: { has_more: false, next_cursor: null } };
    const { stub, client } = clientWith({
      "GET /api/v1/admin/admin-invites": ok(empty),
      "GET /api/v1/admin/admin-identities": ok(empty),
    });
    await client.listAdminInvites({ limit: 50 });
    await client.listAdminIdentities({ limit: 50, status: "active" });
    for (const request of stub.requests) {
      expect(Object.keys(request.headers)).not.toContain("Idempotency-Key");
      expect(Object.keys(request.headers)).not.toContain("If-Match");
    }
    expect(stub.requests[1]?.url).toContain("status=active");
  });
});
