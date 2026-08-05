import { describe, expect, test } from "bun:test";

import { isMoiraRequestError } from "@/lib/errors";
// The namespace import is load-bearing: the guard-invocation block at the foot
// of this file derives the set of guards from this module's OWN exports, so a
// guard added without a pin fails there rather than going unnoticed.
import * as moiraClientModule from "@/lib/moira-client";
import {
  AUTH_PROVIDER_OPERATION_NAMES,
  LLM_CONFIG_OPERATION_NAMES,
  MOIRA_OPERATIONS,
  MoiraClient,
  MoiraClientContractError,
  apiKeyCredentialSecret,
  assertClaimRequestIsSafe,
  assertCredentialCreateIsSafe,
  assertCredentialRotateIsSafe,
  assertLlmProviderCreateIsSafe,
  assertLlmProviderPatchIsSafe,
  assertProviderCreateIsSafe,
  assertProviderModelCreateIsSafe,
  assertRoutingPolicyCreateIsSafe,
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

  test("exactly thirteen of the registry's operations declare a key", () => {
    // Every entry is read off the spec, not assumed;
    // `tests/contract/openapi-contract.test.ts` re-derives each flag from
    // `docs/openapi.json` on every run.
    //
    // FOUR ARRIVED IN WAVE 5, and one of them is a correction to plan 09 §0.8.4
    // step 6, which lists "create, revoke, redeem and delete" for this family.
    // `patch_admin_identity` declares an optional `Idempotency-Key` **as well as**
    // its required `If-Match`, so the audit's list was incomplete. The registry
    // follows the committed spec, not the audit.
    //
    // FIVE MORE ARRIVED WITH ISSUE #73, and the shape of the LLM surface is
    // visible in which ones: every CREATE declares a key, no enable/disable/patch
    // does, and `rotateProviderCredential` declares one ALONGSIDE a required
    // `If-Match` — the only entry here besides `patchAdminIdentity` that carries
    // both.
    const withKey = (Object.keys(MOIRA_OPERATIONS) as MoiraOperationName[])
      .filter((name) => MOIRA_OPERATIONS[name].declaresIdempotencyKey)
      .sort();
    expect(withKey).toEqual([
      "claimAdminIdentity",
      "createAdminInvite",
      "createAuthProvider",
      "createProvider",
      "createProviderCredential",
      "createProviderModel",
      "createRoutingPolicy",
      "createTrustedJwtIssuer",
      "deleteAdminIdentity",
      "patchAdminIdentity",
      "redeemAdminInvite",
      "revokeAdminInvite",
      "rotateProviderCredential",
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

/* -------------------------------------------------------------------------- */
/* LLM runtime configuration (issue #73)                                      */
/* -------------------------------------------------------------------------- */

const providerRecord = {
  id: "prov-1",
  provider_type: "open_ai_compatible",
  display_name: "Local vLLM",
  status: "disabled",
  metadata: {},
  created_at: "2026-08-01T00:00:00Z",
  updated_at: "2026-08-01T00:00:00Z",
  version: 4,
  base_url: "http://192.168.1.13:8000/v1",
};

const credentialRecord = {
  id: "cred-1",
  provider_id: "prov-1",
  credential_type: "api_key",
  scope: { type: "global" },
  secret_fingerprint: "sha256:abc",
  masked_secret: "sk-...9f2",
  status: "active",
  priority: 0,
  metadata: {},
  created_at: "2026-08-01T00:00:00Z",
  updated_at: "2026-08-01T00:00:00Z",
  version: 2,
};

/**
 * Every registered operation that is NOT LLM runtime configuration and NOT an
 * auth-provider operation, spelled out.
 *
 * Issue #113: `LLM_CONFIG_OPERATION_NAMES` is now DERIVED from the registry by
 * path, which removes the drift the issue was about but leaves one question the
 * derivation cannot answer on its own — "is this registry entry LLM
 * configuration, and did anybody decide?". This list is the answer. Together
 * with the two exported name sets it partitions `MOIRA_OPERATIONS` exactly, so
 * registering ANY new operation fails here until somebody classifies it.
 */
const OPERATIONS_OUTSIDE_THE_LLM_AND_AUTH_PROVIDER_SURFACES = [
  "claimAdminIdentity",
  "createAdminInvite",
  "createTrustedJwtIssuer",
  "deleteAdminIdentity",
  "enableTrustedJwtIssuer",
  "getAdminInvite",
  "getSetupAuthMethods",
  "getSetupClaimStatus",
  "getSetupSignInMethods",
  "listAdminIdentities",
  "listAdminInvites",
  "listTrustedJwtIssuers",
  "patchAdminIdentity",
  "previewAdminInvite",
  "redeemAdminInvite",
  "revokeAdminInvite",
] as const satisfies readonly MoiraOperationName[];

describe("the LLM configuration surface is administration, never bootstrap", () => {
  test("the derived set is COMPLETE: every operation is classified exactly once", () => {
    // The completeness assertion issue #113 asked for, kept after the list was
    // derived rather than dropped as redundant. Derivation stops the set going
    // STALE; it cannot stop a family being added at a path nobody thought to
    // include, and an LLM operation missing from this set inherits none of the
    // security assertions below.
    const all = (Object.keys(MOIRA_OPERATIONS) as MoiraOperationName[]).sort();
    const classified = [
      ...LLM_CONFIG_OPERATION_NAMES,
      ...AUTH_PROVIDER_OPERATION_NAMES,
      ...OPERATIONS_OUTSIDE_THE_LLM_AND_AUTH_PROVIDER_SURFACES,
    ].sort();
    expect(
      classified,
      "an operation is registered that no named surface claims, or one is claimed twice. If it " +
        "is LLM runtime configuration, register it under one of the collections " +
        "LLM_CONFIG_OPERATION_NAMES derives from; otherwise add it to the list above.",
    ).toEqual(all);
    // A floor, so an empty or broken derivation cannot make the rules below
    // vacuous by having nothing to iterate.
    expect(LLM_CONFIG_OPERATION_NAMES.length).toBeGreaterThanOrEqual(22);
    // And the derivation still excludes the neighbouring surface whose path only
    // LOOKS like this one's: `/api/v1/admin/auth/providers`.
    for (const name of AUTH_PROVIDER_OPERATION_NAMES) {
      expect(LLM_CONFIG_OPERATION_NAMES, `${name} is not LLM configuration`).not.toContain(name);
    }
  });

  test("every one of its operations requires a credential", () => {
    // The security posture stated as a property of the SET. The setup path is
    // unauthenticated by design — it runs before any admin exists — and none of
    // that reasoning transfers to configuring which model the deployment talks
    // to. An entry that drifted to `credential: "none"` would make a provider or
    // a credential writable by anyone who can reach Moira.
    for (const name of LLM_CONFIG_OPERATION_NAMES) {
      expect(MOIRA_OPERATIONS[name].credential, `${name} must require a credential`).toBe("admin");
    }
    // And the anonymous set is still exactly the three setup/invite operations —
    // asserted here as well as at the top of the file, because THIS is the change
    // that could have added a fourth.
    const anonymous = (Object.keys(MOIRA_OPERATIONS) as MoiraOperationName[]).filter(
      (name) => MOIRA_OPERATIONS[name].credential === "none",
    );
    expect(anonymous.sort()).toEqual([
      "getSetupClaimStatus",
      "getSetupSignInMethods",
      "previewAdminInvite",
    ]);
  });

  test("the reads send the system key and no precondition headers", async () => {
    const empty = { data: [], pagination: { has_more: false, next_cursor: null } };
    const { stub, client } = clientWith({
      "GET /api/v1/admin/providers": ok(empty),
      "GET /api/v1/admin/routes": ok(empty),
      "GET /api/v1/admin/routing-policies": ok(empty),
    });
    await client.listProviders({ limit: 50 });
    await client.listRoutes({ limit: 50 });
    await client.listRoutingPolicies({ limit: 50 });

    expect(stub.requests).toHaveLength(3);
    for (const request of stub.requests) {
      expect(request.headers["X-Moira-System-Key"]).toBe(SYSTEM_KEY);
      expect(Object.keys(request.headers)).not.toContain("Idempotency-Key");
      expect(Object.keys(request.headers)).not.toContain("If-Match");
    }
  });

  test("the routes family is READ-ONLY: no create, patch, enable or delete", () => {
    // `POST /api/v1/admin/routes` exists in the spec and is deliberately absent
    // here — see the registry comment. Asserted over the registry rather than
    // over the client's method names so an operation added without a method still
    // fails.
    const routeOperations = Object.values(MOIRA_OPERATIONS).filter((operation) =>
      operation.path.startsWith("/api/v1/admin/routes"),
    );
    expect(routeOperations.map((operation) => operation.method).sort()).toEqual(["GET", "GET"]);
  });
});

describe("If-Match on the LLM surface", () => {
  test("enable/disable/patch/rotate refuse an empty version, and send nothing", async () => {
    // THE PIN FOR `If-Match`. Written as a refusal that reaches no wire: an
    // assertion that the header is merely absent would also pass if the request
    // were sent without a precondition, which is the failure — a lifecycle move
    // landing on a row that changed under the operator.
    const { stub, client } = clientWith({});
    await expect(client.enableProvider("prov-1", "")).rejects.toThrow(MoiraClientContractError);
    await expect(client.disableProvider("prov-1", "")).rejects.toThrow(MoiraClientContractError);
    await expect(client.patchProvider("prov-1", { display_name: "x" }, "")).rejects.toThrow(
      MoiraClientContractError,
    );
    await expect(client.enableProviderModel("mod-1", "")).rejects.toThrow(MoiraClientContractError);
    await expect(client.disableProviderCredential("cred-1", "")).rejects.toThrow(
      MoiraClientContractError,
    );
    await expect(
      client.rotateProviderCredential("cred-1", { secret: { api_key: "k" } }, ""),
    ).rejects.toThrow(MoiraClientContractError);
    await expect(client.enableRoutingPolicy("pol-1", "")).rejects.toThrow(MoiraClientContractError);
    expect(stub.requests, "the refusal must happen before the request is sent").toEqual([]);
  });

  test("enable sends the version from a prior read, through ifMatchFor", async () => {
    // `ifMatchFor` rather than a second convention: one helper, so "where did
    // this version come from" has one answer everywhere in this client.
    const { stub, client } = clientWith({
      "POST /api/v1/admin/providers/prov-1/enable": ok({ ...providerRecord, status: "active" }),
    });
    await client.enableProvider("prov-1", ifMatchFor(providerRecord));
    expect(stub.requests[0]?.headers["If-Match"]).toBe("4");
    // And no key: `enable_provider` declares none, so retry safety here is the
    // precondition plus enable being naturally idempotent — nothing else.
    expect(Object.keys(stub.requests[0]?.headers ?? {})).not.toContain("Idempotency-Key");
  });

  test("enable cannot carry an Idempotency-Key — reaching for it throws", async () => {
    const { client } = clientWith({});
    expect(MOIRA_OPERATIONS.enableProvider.declaresIdempotencyKey).toBe(false);
    await expect(
      // @ts-expect-error enableProvider deliberately exposes no key parameter
      client.enableProvider("prov-1", "4", { idempotencyKey: "k" }),
    ).rejects.toBeDefined();
  });

  test("rotate carries BOTH the precondition and the key", async () => {
    const { stub, client } = clientWith({
      "POST /api/v1/admin/provider-credentials/cred-1/rotate": ok(credentialRecord),
    });
    await client.rotateProviderCredential(
      "cred-1",
      { secret: apiKeyCredentialSecret("replacement-key") },
      ifMatchFor(credentialRecord),
      { idempotencyKey: "rotate-cred-1-v2" },
    );
    const headers = stub.requests[0]?.headers ?? {};
    expect(headers["If-Match"]).toBe("2");
    expect(headers["Idempotency-Key"]).toBe("rotate-cred-1-v2");
  });

  test("a read declares no If-Match, so none is required", async () => {
    const { client } = clientWith({ "GET /api/v1/admin/providers/prov-1": ok(providerRecord) });
    expect(MOIRA_OPERATIONS.getProvider.requiresIfMatch).toBe(false);
    // Asserted rather than assumed because the neighbouring PATCH on the same
    // path requires one; a caller copying the mutation call would otherwise send
    // a header the operation does not declare.
    await expect(client.getProvider("prov-1")).resolves.toMatchObject({ id: "prov-1" });
  });
});

describe("the LLM provider builder", () => {
  test("an open_ai_compatible provider without a base_url is refused", () => {
    // The one check here that is not a restatement of the schema, and the
    // dangerous one: the compatible arm with no base URL does not fail, it falls
    // back to the vendor default. An operator who meant to reach a machine on
    // their own network sends prompts to a third party and sees a working
    // deployment.
    for (const baseUrl of [undefined, null, ""]) {
      expect(() =>
        assertLlmProviderCreateIsSafe({
          provider_type: "open_ai_compatible",
          display_name: "Local vLLM",
          base_url: baseUrl,
        }),
      ).toThrow(/base_url/);
    }
    expect(() =>
      assertLlmProviderCreateIsSafe({
        provider_type: "open_ai_compatible",
        display_name: "Local vLLM",
        base_url: "http://192.168.1.13:8000/v1",
      }),
    ).not.toThrow();
    // `open_ai` genuinely does not need one — the guard is per-arm, not blanket.
    expect(() =>
      assertLlmProviderCreateIsSafe({ provider_type: "open_ai", display_name: "OpenAI" }),
    ).not.toThrow();
  });

  test("an unknown or missing provider_type is refused", () => {
    for (const providerType of [undefined, null, "", "vllm", "openai"]) {
      expect(() =>
        assertLlmProviderCreateIsSafe({ provider_type: providerType, display_name: "d" }),
      ).toThrow(/provider_type/);
    }
  });

  test("display_name is required", () => {
    expect(() =>
      assertLlmProviderCreateIsSafe({ provider_type: "anthropic", display_name: "" }),
    ).toThrow(/display_name/);
  });

  test("patch refuses provider_type — it is immutable, and the 400 names nothing", () => {
    expect(() => assertLlmProviderPatchIsSafe({ provider_type: "open_ai" })).toThrow(
      /provider_type/,
    );
    // Even `undefined` counts: `"provider_type" in body` is what a hand-built
    // patch body carries once a form has cleared the field.
    expect(() => assertLlmProviderPatchIsSafe({ provider_type: undefined })).toThrow(
      /provider_type/,
    );
    expect(() => assertLlmProviderPatchIsSafe({ display_name: "Renamed" })).not.toThrow();
  });
});

describe("the provider-model builder", () => {
  test("capabilities must be sent explicitly — omitted and null are both refused", () => {
    // The seed-script gotcha, as a client-side refusal. Absent, the column is
    // stored as null, routing's capability filter matches nothing, and the first
    // completion fails `no_eligible_model` — an error that names neither the
    // model nor the missing field.
    expect(() => assertProviderModelCreateIsSafe({ model_key: "Qwen/Qwen3-4B" })).toThrow(
      /capabilities/,
    );
    expect(() =>
      assertProviderModelCreateIsSafe({ model_key: "Qwen/Qwen3-4B", capabilities: null }),
    ).toThrow(/capabilities/);
    // An empty array is a DIFFERENT statement from `null` — "this model declares
    // no capabilities" is a fact an operator can assert — and is allowed.
    expect(() =>
      assertProviderModelCreateIsSafe({ model_key: "Qwen/Qwen3-4B", capabilities: [] }),
    ).not.toThrow();
  });

  test("model_key must be non-empty", () => {
    expect(() => assertProviderModelCreateIsSafe({ model_key: "", capabilities: [] })).toThrow(
      /model_key/,
    );
  });

  test("create posts to the nested path and carries the key", async () => {
    const { stub, client } = clientWith({
      "POST /api/v1/admin/providers/prov-1/models": () => ({
        status: 201,
        body: {
          id: "mod-1",
          provider_id: "prov-1",
          model_key: "Qwen/Qwen3-4B",
          capabilities: ["chat", "tools"],
          status: "active",
          created_at: "2026-08-01T00:00:00Z",
          updated_at: "2026-08-01T00:00:00Z",
          version: 1,
        },
      }),
    });
    await client.createProviderModel(
      "prov-1",
      { model_key: "Qwen/Qwen3-4B", capabilities: ["chat", "tools"] },
      { idempotencyKey: "model-prov-1-qwen3-4b" },
    );
    expect(stub.routes()).toEqual(["POST /api/v1/admin/providers/prov-1/models"]);
    expect(stub.requests[0]?.headers["Idempotency-Key"]).toBe("model-prov-1-qwen3-4b");
    expect(stub.bodyOf("POST /api/v1/admin/providers/prov-1/models")).toEqual({
      model_key: "Qwen/Qwen3-4B",
      capabilities: ["chat", "tools"],
    });
  });
});

describe("the credential builder — the untagged-union trap", () => {
  test("an api_key secret carrying `endpoint` is refused, INCLUDING endpoint: null", () => {
    // `CredentialSecret` is serde-untagged and two arms require `api_key`:
    // `{ api_key }` and `{ api_key, endpoint? }`. A body with both keys satisfies
    // BOTH, so `oneOf` matches twice and the request is refused with no field
    // named. `endpoint: null` is the trap: it looks like being explicit and is a
    // legal value of the azure arm.
    for (const endpoint of [null, undefined, "https://x.openai.azure.com"]) {
      expect(() =>
        assertCredentialCreateIsSafe({
          provider_id: "prov-1",
          credential_type: "api_key",
          scope: { type: "global" },
          secret: { api_key: "k", endpoint },
        }),
      ).toThrow(/endpoint/);
    }
    expect(() =>
      assertCredentialCreateIsSafe({
        provider_id: "prov-1",
        credential_type: "api_key",
        scope: { type: "global" },
        secret: { api_key: "k" },
      }),
    ).not.toThrow();
  });

  test("an empty api_key is refused even against a keyless endpoint", () => {
    // A local vLLM ignores the header, which makes "leave it blank" look
    // reasonable. Routing resolves a credential ROW before it builds a request,
    // so a provider with none fails `credential_not_found` — which reads as "your
    // key is wrong" when the truth is "there is no key".
    expect(() => apiKeyCredentialSecret("")).toThrow(MoiraClientContractError);
    expect(() =>
      assertCredentialCreateIsSafe({
        provider_id: "prov-1",
        credential_type: "api_key",
        scope: { type: "global" },
        secret: { api_key: "" },
      }),
    ).toThrow(/api_key/);
    expect(apiKeyCredentialSecret("not-used-by-vllm")).toEqual({ api_key: "not-used-by-vllm" });
  });

  test("apiKeyCredentialSecret returns exactly one key, whatever it is handed", () => {
    // A fresh literal rather than a spread, so no call site can smuggle
    // `endpoint` in through an object it already had.
    expect(Object.keys(apiKeyCredentialSecret("k"))).toEqual(["api_key"]);
  });

  test("provider_id and scope are required, and the message says why", () => {
    expect(() =>
      assertCredentialCreateIsSafe({
        provider_id: "",
        credential_type: "api_key",
        scope: { type: "global" },
        secret: { api_key: "k" },
      }),
    ).toThrow(/provider_id/);
    expect(() =>
      assertCredentialCreateIsSafe({
        provider_id: "prov-1",
        credential_type: "api_key",
        scope: {},
        secret: { api_key: "k" },
      }),
    ).toThrow(/scope/);
  });

  test("rotate refuses endpoint: null and an empty replacement key", () => {
    expect(() => assertCredentialRotateIsSafe({ secret: { api_key: "k", endpoint: null } })).toThrow(
      /endpoint/,
    );
    expect(() => assertCredentialRotateIsSafe({ secret: { api_key: "" } })).toThrow(/api_key/);
    expect(() => assertCredentialRotateIsSafe({ secret: { api_key: "k2" } })).not.toThrow();
  });

  test("the raw key goes in the body and never in the URL", async () => {
    const { stub, client } = clientWith({
      "POST /api/v1/admin/provider-credentials": () => ({ status: 201, body: credentialRecord }),
    });
    await client.createProviderCredential(
      {
        provider_id: "prov-1",
        credential_type: "api_key",
        scope: { type: "global" },
        secret: apiKeyCredentialSecret("sk-fake-key-for-this-test"),
      },
      { idempotencyKey: "cred-prov-1" },
    );
    const request = stub.requests[0]!;
    // The URL reaches access logs, proxy logs and `Referer` chains. The body does
    // not.
    expect(request.url).not.toContain("sk-fake-key-for-this-test");
    expect(request.body).toEqual({
      provider_id: "prov-1",
      credential_type: "api_key",
      scope: { type: "global" },
      secret: { api_key: "sk-fake-key-for-this-test" },
    });
  });

  test("listing filters by provider server-side rather than in the console", async () => {
    // Filtering here rather than listing everything and matching locally keeps
    // other providers' credential rows out of this process entirely.
    const { stub, client } = clientWith({
      "GET /api/v1/admin/provider-credentials": ok({
        data: [],
        pagination: { has_more: false, next_cursor: null },
      }),
    });
    await client.listProviderCredentials({ providerId: "prov-1", limit: 100 });
    expect(stub.requests[0]?.url).toContain("provider_id=prov-1");
  });
});

describe("the routing-policy builder", () => {
  test("the three foreign keys are required and the message names the risk", () => {
    for (const field of ["route_id", "provider_id", "provider_model_id"]) {
      const body: Record<string, unknown> = {
        route_id: "route-1",
        provider_id: "prov-1",
        provider_model_id: "mod-1",
      };
      body[field] = "";
      expect(() => assertRoutingPolicyCreateIsSafe(body)).toThrow(new RegExp(field));
    }
    expect(() =>
      assertRoutingPolicyCreateIsSafe({
        route_id: "route-1",
        provider_id: "prov-1",
        provider_model_id: "mod-1",
      }),
    ).not.toThrow();
  });

  test("findRouteByKey matches exactly — a prefix is not a match", async () => {
    const routeRow = (routeKey: string) => ({
      id: `route-${routeKey}`,
      route_key: routeKey,
      display_name: routeKey,
      status: "active",
      selection_strategy: "default",
      metadata: {},
      created_at: "2026-08-01T00:00:00Z",
      updated_at: "2026-08-01T00:00:00Z",
      version: 1,
    });
    const { client } = clientWith({
      "GET /api/v1/admin/routes": ok({
        data: [routeRow("general-fallback")],
        pagination: { has_more: false, next_cursor: null },
      }),
    });
    // Binding a policy to a prefix-matched route would be worse than finding
    // none: it would look like it worked.
    expect(await client.findRouteByKey("general")).toBeNull();
  });
});

describe("a refused LLM write maps through lib/errors.ts like every other failure", () => {
  test("409 becomes a MoiraRequestError whose remedy is resolve_conflict", async () => {
    // THE PIN FOR ERROR MAPPING. `#request` calls `toMoiraError` under
    // `if (!response.ok)` and nothing else — these methods add no catch, no
    // rewrite and no logging — so a refused write on this surface reaches the
    // caller through the same union as a refused write anywhere else. A method
    // that swallowed the conflict and returned a record would be invisible to
    // every other test in this file.
    const { client } = clientWith({
      "POST /api/v1/admin/providers/prov-1/enable": () => ({
        status: 409,
        body: errorEnvelope("resource_version_conflict"),
      }),
    });

    const caught = await client
      .enableProvider("prov-1", ifMatchFor(providerRecord))
      .catch((error: unknown) => error);
    expect(isMoiraRequestError(caught)).toBe(true);
    if (!isMoiraRequestError(caught)) throw new Error("unreachable");
    expect(caught.moiraError.kind).toBe("api");
    expect(caught.moiraError.remedy).toBe("resolve_conflict");
    // `details` and `request_id` do not cross the boundary, even on the throw
    // path out of a credential-adjacent call.
    expect(JSON.stringify(caught.moiraError)).not.toContain("must not cross the boundary");
  });

  test("a refused credential write never echoes the submitted key", async () => {
    // The error path is the one place a request body classically leaks: a client
    // that attached "the request that failed" to its error would put a raw API
    // key into every log that catches it.
    const { client } = clientWith({
      "POST /api/v1/admin/provider-credentials": () => ({
        status: 422,
        body: errorEnvelope("credential_invalid"),
      }),
    });

    const caught = await client
      .createProviderCredential({
        provider_id: "prov-1",
        credential_type: "api_key",
        scope: { type: "global" },
        secret: apiKeyCredentialSecret("sk-fake-key-for-this-test"),
      })
      .catch((error: unknown) => error);
    expect(isMoiraRequestError(caught)).toBe(true);
    if (!isMoiraRequestError(caught)) throw new Error("unreachable");
    expect(JSON.stringify(caught.moiraError)).not.toContain("sk-fake-key-for-this-test");
    expect(String((caught as Error).stack ?? "")).not.toContain("sk-fake-key-for-this-test");
  });
});

/* -------------------------------------------------------------------------- */
/* THE GUARDS ARE INVOKED, NOT MERELY EXPORTED (issue #113)                    */
/* -------------------------------------------------------------------------- */

/**
 * ============================================================================
 * WHAT THIS BLOCK EXISTS FOR
 * ============================================================================
 *
 * Every guard above is tested by CALLING IT DIRECTLY. Not one of those tests can
 * observe whether the client method that is supposed to call it still does —
 * they construct their own inputs, exactly as finding F25 described. Delete
 * `assertProviderModelCreateIsSafe(...)` from `createProviderModel`'s body and
 * every gate in this repo stayed green: the guard's own tests kept passing, and
 * the method's tests only ever fed it VALID bodies.
 *
 * `tests/unit/architecture/guard-reachability.test.ts` cannot close this either,
 * and says so about `readIdpSubject` for the same reason: these guards are
 * defined and called inside `lib/moira-client.ts` alone, so its rule — "called
 * from a file that is not the defining one" — can never apply to them.
 *
 * ============================================================================
 * HOW EACH PIN IS BUILT SO IT REDS ON EXACTLY THAT DELETION
 * ============================================================================
 *
 * Each row hands its method a body that ONLY its guard refuses, against a stub
 * that ANSWERS THAT ROUTE SUCCESSFULLY. That second half is what makes the
 * failure unambiguous:
 *
 *   guard present -> rejects with MoiraClientContractError, nothing on the wire
 *   guard deleted -> the request is sent, the stub answers 2xx, the call RESOLVES
 *
 * so a deleted guard fails on the rejection assertion rather than on some
 * incidental transport error a hostile stub would also have produced.
 * `stub.requests` is then asserted EMPTY: a guard that threw after the request
 * had already gone out would satisfy the rejection and still have written.
 *
 * The `ifMatch` arguments below are deliberately non-empty. `#buildHeaders`
 * refuses an empty precondition, and that refusal is ALSO a
 * `MoiraClientContractError` — a pin that let it fire would pass with the guard
 * gone.
 */

interface GuardPin {
  /** The exported guard whose call site is being pinned. */
  readonly guard: string;
  /** The client method that must call it. */
  readonly method: string;
  /** The route the stub answers successfully, and that must stay untouched. */
  readonly route: string;
  /** The body only this guard refuses, sent through the real method. */
  readonly call: (client: MoiraClient) => Promise<unknown>;
  /** What ships if the call site is gone, quoted in the failure message. */
  readonly consequence: string;
}

const GUARD_PINS: readonly GuardPin[] = [
  {
    guard: "assertClaimRequestIsSafe",
    method: "claimAdminIdentity",
    route: "POST /api/v1/admin/setup/claim",
    call: (client) =>
      client.claimAdminIdentity({
        email: "ops@example.com",
        email_verified: true,
        scopes: [],
      } as unknown as Parameters<MoiraClient["claimAdminIdentity"]>[0]),
    consequence:
      "`scopes: []` reaches Moira and creates a permanent admin grant with zero scopes — a no-op " +
      "admin that no retry can revoke",
  },
  {
    guard: "assertTrustedIssuerCreateIsSafe",
    method: "createTrustedJwtIssuer",
    route: "POST /api/v1/admin/jwt-issuers",
    call: (client) =>
      client.createTrustedJwtIssuer({
        issuer: "https://console.example.com",
        scopes_claim: "scopes",
      } as unknown as Parameters<MoiraClient["createTrustedJwtIssuer"]>[0]),
    consequence:
      "the console's own issuer is created able to self-assert scopes, displacing admin_identities " +
      "as the source of human authorization",
  },
  {
    guard: "assertProviderCreateIsSafe",
    method: "createAuthProvider",
    route: "POST /api/v1/admin/auth/providers",
    call: (client) =>
      client.createAuthProvider({
        display_name: "Console IdP",
        trusted_jwt_issuer_id: "issuer-1",
        enabled: false,
      } as unknown as Parameters<MoiraClient["createAuthProvider"]>[0]),
    consequence:
      "`enabled` is sent on create, so the row's lifecycle no longer has enableAuthProvider() as " +
      "its single commit point",
  },
  {
    guard: "assertLlmProviderCreateIsSafe",
    method: "createProvider",
    route: "POST /api/v1/admin/providers",
    call: (client) =>
      client.createProvider({
        provider_type: "open_ai_compatible",
        display_name: "Local vLLM",
      } as unknown as Parameters<MoiraClient["createProvider"]>[0]),
    consequence:
      "an open_ai_compatible provider is created with no base_url, silently falls back to the " +
      "vendor's public API, and sends prompts the operator believed were local to a third party",
  },
  {
    guard: "assertLlmProviderPatchIsSafe",
    method: "patchProvider",
    route: "PATCH /api/v1/admin/providers/prov-1",
    call: (client) =>
      client.patchProvider(
        "prov-1",
        { provider_type: "open_ai" } as unknown as Parameters<MoiraClient["patchProvider"]>[1],
        "4",
      ),
    consequence:
      "an immutable field is PATCHed, and additionalProperties:false answers a flat 400 naming " +
      "nothing — a validation failure shown for a request that is impossible, not invalid",
  },
  {
    guard: "assertProviderModelCreateIsSafe",
    method: "createProviderModel",
    route: "POST /api/v1/admin/providers/prov-1/models",
    call: (client) =>
      client.createProviderModel("prov-1", {
        model_key: "Qwen/Qwen3-4B",
      } as unknown as Parameters<MoiraClient["createProviderModel"]>[1]),
    consequence:
      "capabilities is stored as SQL null, routing's capability filter matches the row against " +
      "nothing, and the first completion fails `no_eligible_model` naming neither model nor field",
  },
  {
    guard: "assertCredentialCreateIsSafe",
    method: "createProviderCredential",
    route: "POST /api/v1/admin/provider-credentials",
    call: (client) =>
      client.createProviderCredential({
        provider_id: "prov-1",
        credential_type: "api_key",
        scope: { type: "global" },
        secret: { api_key: "k", endpoint: null },
      } as unknown as Parameters<MoiraClient["createProviderCredential"]>[0]),
    consequence:
      "`endpoint: null` makes the untagged CredentialSecret match two arms at once and the request " +
      "is refused as ambiguous, with no field named",
  },
  {
    guard: "assertCredentialRotateIsSafe",
    method: "rotateProviderCredential",
    route: "POST /api/v1/admin/provider-credentials/cred-1/rotate",
    call: (client) =>
      client.rotateProviderCredential(
        "cred-1",
        { secret: { api_key: "k2", endpoint: null } } as unknown as Parameters<
          MoiraClient["rotateProviderCredential"]
        >[1],
        "2",
      ),
    consequence: "the same untagged-union ambiguity, on the request that replaces a live key",
  },
  {
    guard: "assertRoutingPolicyCreateIsSafe",
    method: "createRoutingPolicy",
    route: "POST /api/v1/admin/routing-policies",
    call: (client) =>
      client.createRoutingPolicy({
        route_id: "",
        provider_id: "prov-1",
        provider_model_id: "mod-1",
      } as unknown as Parameters<MoiraClient["createRoutingPolicy"]>[0]),
    consequence:
      "a policy is created with an empty foreign key on the one write that decides which provider " +
      "live traffic reaches",
  },
];

describe("every input guard is actually invoked by the method that owns it", () => {
  test("the pinned set is EXACTLY the guards this module exports", () => {
    // The completeness half. A tenth guard added without a pin here would ship
    // with a call site nothing can see, which is the state issue #113 found the
    // first nine in. Derived from the module's own exports rather than typed
    // out, so it cannot be satisfied by editing a list.
    const exported = Object.keys(moiraClientModule)
      .filter((name) => /^assert[A-Za-z]*IsSafe$/.test(name))
      .sort();
    expect(exported.length, "no guards were discovered — this rule would pass on nothing").toBe(9);
    expect(
      GUARD_PINS.map((pin) => pin.guard).sort(),
      "a guard is exported with no pin below. Add one: a body only that guard refuses, sent " +
        "through the method that must call it, against a stub that would ANSWER it successfully.",
    ).toEqual(exported);
  });

  for (const pin of GUARD_PINS) {
    test(`${pin.method}() calls ${pin.guard}()`, async () => {
      // The stub SUCCEEDS on this route. With the call site deleted the request
      // is sent, this resolves, and the assertion below fails — which is the
      // whole design of this test.
      const { stub, client } = clientWith({
        [pin.route]: () => ({ status: 200, body: { id: "irrelevant-to-this-assertion" } }),
      });

      const caught = await pin.call(client).then(
        () => null,
        (error: unknown) => error,
      );

      expect(
        caught instanceof MoiraClientContractError,
        `${pin.method}() did not refuse a body that ${pin.guard}() refuses, so the guard call is ` +
          `gone from its body. Consequence: ${pin.consequence}.`,
      ).toBe(true);
      expect(
        stub.requests.map((request) => request.route),
        `${pin.method}() reached Moira before refusing. A guard that throws after the write has ` +
          "already gone out is not a guard.",
      ).toEqual([]);
    });
  }
});
