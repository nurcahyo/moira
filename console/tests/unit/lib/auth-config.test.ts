// Resolving the two halves of the auth configuration into one usable provider —
// and, more importantly, refusing to when they do not agree.

import { describe, expect, test } from "bun:test";

import {
  authConfigCacheKey,
  CONSOLE_OAUTH_PROVIDER_ID,
  hasUsableEndpoints,
  isEmailDomainAllowed,
  isInteractiveMethod,
  loadAuthConfig,
  resolveAuthConfig,
} from "@/lib/auth-config";
import { InMemoryConsoleSecretStore, sealClientSecret } from "@/lib/console-secrets";
import { MoiraClient } from "@/lib/moira-client";
import type { AuthProviderSettingsRecord } from "@/lib/types";

import { createMoiraStub, MOIRA_STUB_BASE_URL } from "../../support/moira-stub";

const KEY = Buffer.alloc(32, 0x11);
const CLIENT_ID = "console.apps.idp.test";
const SECRET = "the-client-secret";

function providerRow(overrides: Partial<AuthProviderSettingsRecord> = {}): AuthProviderSettingsRecord {
  return {
    id: "11111111-1111-4111-8111-111111111111",
    method: "generic_oidc",
    display_name: "Corporate IdP",
    enabled: true,
    requested_scopes: ["openid", "email"],
    allowed_email_domains: ["example.com"],
    allowed_algorithms: ["ES256"],
    expected_audiences: [],
    redirect_uris: [],
    metadata: null,
    status: "active",
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    version: 4,
    discovery_url: "https://idp.example.com/.well-known/openid-configuration",
    client_id: CLIENT_ID,
    trusted_jwt_issuer_id: "22222222-2222-4222-8222-222222222222",
    ...overrides,
  };
}

const sealedFor = (clientId = CLIENT_ID) =>
  sealClientSecret(KEY, providerRow().id, clientId, SECRET);

/* -------------------------------------------------------------------------- */
/* The cache key — a §0 correction                                            */
/* -------------------------------------------------------------------------- */

describe("authConfigCacheKey", () => {
  test("changes when a row's version is bumped", () => {
    const before = authConfigCacheKey([{ id: "a", version: 1 }], null);
    const after = authConfigCacheKey([{ id: "a", version: 2 }], null);
    expect(before).not.toBe(after);
  });

  test("changes when a row is DELETED — the case `max(version)` cannot see", () => {
    // This is the whole reason the key is a digest over the set rather than a
    // maximum. Deleting the LOWER-versioned of two rows leaves `max(version)`
    // completely unchanged, so a `max`-based key would keep serving a
    // configuration that no longer exists.
    const both = authConfigCacheKey(
      [
        { id: "a", version: 1 },
        { id: "b", version: 9 },
      ],
      null,
    );
    const afterDeletingTheLowerOne = authConfigCacheKey([{ id: "b", version: 9 }], null);
    expect(both).not.toBe(afterDeletingTheLowerOne);
  });

  test("changes when a row is ADDED at a lower version", () => {
    const one = authConfigCacheKey([{ id: "b", version: 9 }], null);
    const two = authConfigCacheKey(
      [
        { id: "b", version: 9 },
        { id: "a", version: 1 },
      ],
      null,
    );
    expect(one).not.toBe(two);
  });

  test("is stable under list reordering", () => {
    const ascending = authConfigCacheKey(
      [
        { id: "a", version: 1 },
        { id: "b", version: 2 },
      ],
      null,
    );
    const descending = authConfigCacheKey(
      [
        { id: "b", version: 2 },
        { id: "a", version: 1 },
      ],
      null,
    );
    expect(ascending).toBe(descending);
  });

  test("changes when the console-side secret is rotated", () => {
    const before = authConfigCacheKey([{ id: "a", version: 1 }], "2026-01-01T00:00:00.000Z");
    const after = authConfigCacheKey([{ id: "a", version: 1 }], "2026-02-01T00:00:00.000Z");
    expect(before).not.toBe(after);
  });

  test("cannot be collided by shifting characters between id and version", () => {
    // Length-prefixing is what stops ("ab", 1) and ("a", "b1") hashing alike.
    expect(authConfigCacheKey([{ id: "ab", version: 1 }], null)).not.toBe(
      authConfigCacheKey([{ id: "a", version: 11 }], null),
    );
  });
});

/* -------------------------------------------------------------------------- */
/* Resolution                                                                 */
/* -------------------------------------------------------------------------- */

describe("resolveAuthConfig", () => {
  test("resolves a well-formed row plus a matching console secret", () => {
    const result = resolveAuthConfig([providerRow()], sealedFor(), SECRET, null);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.config.providerId).toBe(CONSOLE_OAUTH_PROVIDER_ID);
    expect(result.config.clientSecret).toBe(SECRET);
    expect(result.config.issuer).toBeNull();
    expect(result.config.allowedEmailDomains).toEqual(["example.com"]);
  });

  test("lower-cases the allow-list so comparison is case-insensitive", () => {
    const result = resolveAuthConfig(
      [providerRow({ allowed_email_domains: ["Example.COM"] })],
      sealedFor(),
      SECRET,
      null,
    );
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.config.allowedEmailDomains).toEqual(["example.com"]);
  });

  test("no enabled provider is a first-run state, not a crash", () => {
    const result = resolveAuthConfig([providerRow({ enabled: false })], null, null, null);
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("no_enabled_provider");
  });

  test("a disabled-by-status row does not count as enabled", () => {
    const result = resolveAuthConfig(
      [providerRow({ enabled: true, status: "deleted" })],
      null,
      null,
      null,
    );
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("no_enabled_provider");
  });

  test("two enabled providers is refused rather than silently resolved", () => {
    // Moira permits several and picks one by a documented ordering at claim
    // time. Guessing the same way here would make the sign-in button depend on
    // row order.
    const result = resolveAuthConfig(
      [providerRow(), providerRow({ id: "33333333-3333-4333-8333-333333333333" })],
      sealedFor(),
      SECRET,
      null,
    );
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("ambiguous_enabled_providers");
  });

  test("`jwks` is not an interactive sign-in method", () => {
    const result = resolveAuthConfig(
      [providerRow({ method: "jwks" })],
      sealedFor(),
      SECRET,
      null,
    );
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("method_not_interactive");
  });

  test("a row with neither discovery nor an authorize/token pair cannot drive a flow", () => {
    const result = resolveAuthConfig(
      [providerRow({ discovery_url: null, authorization_url: null, token_url: null })],
      sealedFor(),
      SECRET,
      null,
    );
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("provider_endpoints_incomplete");
  });

  test("an empty allow-list denies every claim, so it is refused up front", () => {
    const result = resolveAuthConfig(
      [providerRow({ allowed_email_domains: [] })],
      sealedFor(),
      SECRET,
      null,
    );
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("allowed_email_domains_empty");
  });

  test("THE B1 DEFECT, caught on the read path too", () => {
    // A row with no `trusted_jwt_issuer_id` can be signed into and can never
    // produce a successful claim: `governing_policy` matches neither branch, so
    // every claim is 403 admin_claim_domain_not_allowed. Catching it here means
    // the operator learns before the last step of the wizard rather than after.
    const result = resolveAuthConfig(
      [providerRow({ trusted_jwt_issuer_id: null })],
      sealedFor(),
      SECRET,
      null,
    );
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("provider_not_bound_to_trusted_jwt_issuer");
  });

  test("D7 drift: Moira has the provider, the console has no secret", () => {
    const result = resolveAuthConfig([providerRow()], null, null, null);
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("console_secret_unavailable");
    expect(result.drift).toBe("console_secret_missing");
  });

  test("D7 drift: the client id was rotated in Moira but not in the console", () => {
    const result = resolveAuthConfig(
      [providerRow({ client_id: "rotated.apps.idp.test" })],
      sealedFor(),
      SECRET,
      null,
    );
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("console_secret_unavailable");
    expect(result.drift).toBe("client_id_mismatch");
  });

  test("the resolved config never carries a message key on success", () => {
    const result = resolveAuthConfig([providerRow()], sealedFor(), SECRET, null);
    expect(result.ok).toBe(true);
    expect(JSON.stringify(result)).not.toContain("console.error");
  });
});

/* -------------------------------------------------------------------------- */
/* loadAuthConfig — the Moira round trip                                      */
/* -------------------------------------------------------------------------- */

describe("loadAuthConfig", () => {
  test("reads the provider list and marries it to the console's own secret", async () => {
    const stub = createMoiraStub({
      "GET /api/v1/admin/auth/providers": () => ({
        status: 200,
        body: { data: [providerRow()], pagination: { has_more: false, next_cursor: null } },
      }),
    });
    const client = new MoiraClient({
      baseUrl: MOIRA_STUB_BASE_URL,
      systemKey: "sk_test",
      fetch: stub.fetch,
    });
    const store = new InMemoryConsoleSecretStore(KEY);
    await store.put(providerRow().id, CLIENT_ID, SECRET);

    const result = await loadAuthConfig(client, store);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.config.clientSecret).toBe(SECRET);
    expect(stub.routes()).toEqual(["GET /api/v1/admin/auth/providers"]);
  });

  test("does not reveal a secret for a provider it is not going to use", async () => {
    // Two enabled rows is `ambiguous_enabled_providers`; no plaintext should be
    // pulled out of the store on the way to saying so.
    let revealed = 0;
    const stub = createMoiraStub({
      "GET /api/v1/admin/auth/providers": () => ({
        status: 200,
        body: {
          data: [providerRow(), providerRow({ id: "33333333-3333-4333-8333-333333333333" })],
          pagination: { has_more: false, next_cursor: null },
        },
      }),
    });
    const client = new MoiraClient({
      baseUrl: MOIRA_STUB_BASE_URL,
      systemKey: "sk_test",
      fetch: stub.fetch,
    });
    const store = new InMemoryConsoleSecretStore(KEY);
    await store.put(providerRow().id, CLIENT_ID, SECRET);
    const counting = {
      ...store,
      reveal: async (id: string) => {
        revealed += 1;
        return store.reveal(id);
      },
      read: store.read.bind(store),
      put: store.put.bind(store),
      remove: store.remove.bind(store),
      newestUpdatedAt: store.newestUpdatedAt.bind(store),
    };

    const result = await loadAuthConfig(client, counting);
    expect(result.ok).toBe(false);
    expect(revealed).toBe(0);
  });
});

/* -------------------------------------------------------------------------- */
/* Predicates                                                                 */
/* -------------------------------------------------------------------------- */

describe("isEmailDomainAllowed", () => {
  test("matches the domain half, case-insensitively", () => {
    expect(isEmailDomainAllowed("Operator@Example.COM", ["example.com"])).toBe(true);
    expect(isEmailDomainAllowed("operator@example.com", ["EXAMPLE.COM"])).toBe(true);
  });

  test("is NOT a suffix match", () => {
    // `evilexample.com` passing an `example.com` allow-list is the classic
    // failure of a `.endsWith()` implementation.
    expect(isEmailDomainAllowed("attacker@evilexample.com", ["example.com"])).toBe(false);
    expect(isEmailDomainAllowed("attacker@example.com.evil.test", ["example.com"])).toBe(false);
  });

  test("denies by default on an empty allow-list", () => {
    expect(isEmailDomainAllowed("operator@example.com", [])).toBe(false);
  });

  test("rejects malformed addresses rather than matching them", () => {
    expect(isEmailDomainAllowed("no-at-sign", ["example.com"])).toBe(false);
    expect(isEmailDomainAllowed("@example.com", ["example.com"])).toBe(false);
    expect(isEmailDomainAllowed("operator@", ["example.com"])).toBe(false);
  });

  test("uses the LAST @, as an address with a quoted local part requires", () => {
    expect(isEmailDomainAllowed('"weird@local"@example.com', ["example.com"])).toBe(true);
  });
});

describe("endpoint and method predicates", () => {
  test("a discovery URL alone is enough", () => {
    expect(hasUsableEndpoints(providerRow({ authorization_url: null, token_url: null }))).toBe(true);
  });

  test("an authorize/token pair alone is enough", () => {
    expect(
      hasUsableEndpoints(
        providerRow({
          discovery_url: null,
          authorization_url: "https://idp.example.com/authorize",
          token_url: "https://idp.example.com/token",
        }),
      ),
    ).toBe(true);
  });

  test("half a pair is not enough", () => {
    expect(
      hasUsableEndpoints(
        providerRow({
          discovery_url: null,
          authorization_url: "https://idp.example.com/authorize",
          token_url: null,
        }),
      ),
    ).toBe(false);
  });

  test("only google_oauth and generic_oidc are interactive", () => {
    expect(isInteractiveMethod("google_oauth")).toBe(true);
    expect(isInteractiveMethod("generic_oidc")).toBe(true);
    expect(isInteractiveMethod("jwks")).toBe(false);
  });
});
