// Resolving the two halves of the auth configuration into usable providers —
// and, more importantly, refusing to when they do not agree.

import { describe, expect, test } from "bun:test";

import {
  ambiguityGuard,
  authConfigCacheKey,
  CONSOLE_ISSUER_PATH_PREFIX,
  CONSOLE_OAUTH_PROVIDER_ID,
  consoleIssuerForSlug,
  consoleProviderIdFor,
  hasUsableEndpoints,
  isEmailDomainAllowed,
  isInteractiveMethod,
  isProviderSlug,
  loadAuthConfigs,
  resolveAuthConfigs,
  type AuthConfigsInput,
} from "@/lib/auth-config";
import { InMemoryConsoleSecretStore, sealClientSecret } from "@/lib/console-secrets";
import { MoiraClient } from "@/lib/moira-client";
import type { AuthProviderSettingsRecord, TrustedJwtIssuerRecord } from "@/lib/types";

import { createMoiraStub, MOIRA_STUB_BASE_URL } from "../../support/moira-stub";

const KEY = Buffer.alloc(32, 0x11);
const CLIENT_ID = "console.apps.idp.test";
const SECRET = "the-client-secret";

/** The console's own issuer. The incumbent provider mints exactly this. */
const BFF_ISSUER = "https://console.example.com";
const ISSUER_ROW_ID = "22222222-2222-4222-8222-222222222222";
const SECOND_ISSUER_ROW_ID = "44444444-4444-4444-8444-444444444444";
const SECOND_PROVIDER_ROW_ID = "33333333-3333-4333-8333-333333333333";

function providerRow(
  overrides: Partial<AuthProviderSettingsRecord> = {},
): AuthProviderSettingsRecord {
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
    trusted_jwt_issuer_id: ISSUER_ROW_ID,
    ...overrides,
  };
}

function issuerRow(overrides: Partial<TrustedJwtIssuerRecord> = {}): TrustedJwtIssuerRecord {
  return {
    id: ISSUER_ROW_ID,
    issuer: BFF_ISSUER,
    jwks_url: `${BFF_ISSUER}/api/auth/.well-known/jwks.json`,
    expected_audiences: ["moira-admin-api"],
    allowed_algorithms: ["ES256"],
    subject_claim: "sub",
    clock_skew_seconds: 60,
    allow_delegation: false,
    status: "active",
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

const sealedFor = (rowId: string, clientId = CLIENT_ID) =>
  sealClientSecret(KEY, rowId, clientId, SECRET);

/** The whole input, with the well-formed single-provider case as the default. */
function input(overrides: Partial<AuthConfigsInput> = {}): AuthConfigsInput {
  const rows = overrides.rows ?? [providerRow()];
  return {
    rows,
    trustedIssuers: [issuerRow()],
    bffIssuerUrl: BFF_ISSUER,
    sealed: new Map(rows.map((row) => [row.id, sealedFor(row.id)])),
    secrets: new Map(rows.map((row) => [row.id, SECRET])),
    newestSecretUpdatedAt: null,
    ...overrides,
  };
}

/* -------------------------------------------------------------------------- */
/* The cache key — a §0 correction                                            */
/* -------------------------------------------------------------------------- */

describe("authConfigCacheKey", () => {
  test("changes when a row's version is bumped", () => {
    const before = authConfigCacheKey([{ id: "a", version: 1 }], [], null);
    const after = authConfigCacheKey([{ id: "a", version: 2 }], [], null);
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
      [],
      null,
    );
    const afterDeletingTheLowerOne = authConfigCacheKey([{ id: "b", version: 9 }], [], null);
    expect(both).not.toBe(afterDeletingTheLowerOne);
  });

  test("changes when a row is ADDED at a lower version", () => {
    const one = authConfigCacheKey([{ id: "b", version: 9 }], [], null);
    const two = authConfigCacheKey(
      [
        { id: "b", version: 9 },
        { id: "a", version: 1 },
      ],
      [],
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
      [{ id: "i", version: 1 }],
      null,
    );
    const descending = authConfigCacheKey(
      [
        { id: "b", version: 2 },
        { id: "a", version: 1 },
      ],
      [{ id: "i", version: 1 }],
      null,
    );
    expect(ascending).toBe(descending);
  });

  test("changes when the console-side secret is rotated", () => {
    const before = authConfigCacheKey([{ id: "a", version: 1 }], [], "2026-01-01T00:00:00.000Z");
    const after = authConfigCacheKey([{ id: "a", version: 1 }], [], "2026-02-01T00:00:00.000Z");
    expect(before).not.toBe(after);
  });

  test("WAVE 4B: changes when a trusted issuer moves", () => {
    // The minted `iss` is now READ from the trusted-issuer row, so a digest
    // blind to those rows would keep an instance alive that mints the previous
    // issuer string — which is the wrong `admin_identities` grant namespace,
    // not merely a stale display name.
    const before = authConfigCacheKey([{ id: "a", version: 1 }], [{ id: "i", version: 1 }], null);
    const after = authConfigCacheKey([{ id: "a", version: 1 }], [{ id: "i", version: 2 }], null);
    expect(before).not.toBe(after);
  });

  test("cannot be collided by shifting characters between id and version", () => {
    // Length-prefixing is what stops ("ab", 1) and ("a", "b1") hashing alike.
    expect(authConfigCacheKey([{ id: "ab", version: 1 }], [], null)).not.toBe(
      authConfigCacheKey([{ id: "a", version: 11 }], [], null),
    );
  });
});

/* -------------------------------------------------------------------------- */
/* Provider identity derivation (T7)                                          */
/* -------------------------------------------------------------------------- */

describe("the console issuer and the provider id are derived from one stable string", () => {
  test("the incumbent keeps `moira-console-idp` and the deployment's own issuer", () => {
    // The definition, not a special case: the incumbent is whatever provider is
    // bound to the `bffIssuerUrl` trusted issuer.
    expect(consoleProviderIdFor(BFF_ISSUER, BFF_ISSUER)).toBe(CONSOLE_OAUTH_PROVIDER_ID);
  });

  test("a trailing slash on either side does not mint a second identity", () => {
    expect(consoleProviderIdFor(`${BFF_ISSUER}/`, BFF_ISSUER)).toBe(CONSOLE_OAUTH_PROVIDER_ID);
    expect(consoleProviderIdFor(BFF_ISSUER, `${BFF_ISSUER}/`)).toBe(CONSOLE_OAUTH_PROVIDER_ID);
  });

  test("an additional provider round-trips slug -> issuer -> provider id", () => {
    const issuer = consoleIssuerForSlug(BFF_ISSUER, "contractors");
    expect(issuer).toBe(`${BFF_ISSUER}${CONSOLE_ISSUER_PATH_PREFIX}contractors`);
    expect(consoleProviderIdFor(BFF_ISSUER, issuer)).toBe(`${CONSOLE_OAUTH_PROVIDER_ID}-contractors`);
  });

  test("an issuer outside the console's namespace is REFUSED, never guessed at", () => {
    // Inventing an id here would produce a value that changes the next time this
    // derivation is touched — and `account.providerId` cannot be migrated once a
    // human has signed in.
    expect(consoleProviderIdFor(BFF_ISSUER, "https://someone-elses.example/idp/x")).toBeNull();
    expect(consoleProviderIdFor(BFF_ISSUER, `${BFF_ISSUER}/other/x`)).toBeNull();
    // A slug with a path separator would escape its segment in the redirect URI.
    expect(consoleProviderIdFor(BFF_ISSUER, `${BFF_ISSUER}${CONSOLE_ISSUER_PATH_PREFIX}a/b`)).toBeNull();
    expect(consoleProviderIdFor(BFF_ISSUER, `${BFF_ISSUER}${CONSOLE_ISSUER_PATH_PREFIX}`)).toBeNull();
  });

  test("distinct slugs never collide on one provider id", () => {
    const ids = ["a", "b", "corp", "corp-eu"].map((slug) =>
      consoleProviderIdFor(BFF_ISSUER, consoleIssuerForSlug(BFF_ISSUER, slug)),
    );
    expect(new Set(ids).size).toBe(ids.length);
    // And none of them can be mistaken for the incumbent.
    expect(ids).not.toContain(CONSOLE_OAUTH_PROVIDER_ID);
  });

  test("the slug shape is narrow enough to be a URL path segment", () => {
    expect(isProviderSlug("contractors")).toBe(true);
    expect(isProviderSlug("corp-eu-2")).toBe(true);
    expect(isProviderSlug("a")).toBe(true);
    for (const bad of ["", "-lead", "trail-", "Upper", "with space", "a/b", "a..b", "x".repeat(33)]) {
      expect(isProviderSlug(bad), `"${bad}" must not be a usable slug`).toBe(false);
    }
  });

  test("building an issuer from an unusable slug throws rather than emitting one", () => {
    expect(() => consoleIssuerForSlug(BFF_ISSUER, "Bad Slug")).toThrow();
  });
});

/* -------------------------------------------------------------------------- */
/* Resolution                                                                 */
/* -------------------------------------------------------------------------- */

describe("resolveAuthConfigs", () => {
  test("resolves a well-formed row plus a matching console secret", () => {
    const result = resolveAuthConfigs(input());
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.configs).toHaveLength(1);
    const config = result.configs[0]!;
    expect(config.providerId).toBe(CONSOLE_OAUTH_PROVIDER_ID);
    expect(config.consoleIssuer).toBe(BFF_ISSUER);
    expect(config.clientSecret).toBe(SECRET);
    expect(config.issuer).toBeNull();
    expect(config.allowedEmailDomains).toEqual(["example.com"]);
    expect(result.problems).toEqual([]);
  });

  test("lower-cases the allow-list so comparison is case-insensitive", () => {
    const result = resolveAuthConfigs(
      input({ rows: [providerRow({ allowed_email_domains: ["Example.COM"] })] }),
    );
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.configs[0]!.allowedEmailDomains).toEqual(["example.com"]);
  });

  test("no enabled provider is a first-run state, not a crash", () => {
    const result = resolveAuthConfigs(input({ rows: [providerRow({ enabled: false })] }));
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("no_enabled_provider");
  });

  test("a disabled-by-status row does not count as enabled", () => {
    const result = resolveAuthConfigs(
      input({ rows: [providerRow({ enabled: true, status: "deleted" })] }),
    );
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("no_enabled_provider");
  });

  test("`jwks` is not an interactive sign-in method", () => {
    const result = resolveAuthConfigs(input({ rows: [providerRow({ method: "jwks" })] }));
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("method_not_interactive");
  });

  test("a row with neither discovery nor an authorize/token pair cannot drive a flow", () => {
    const result = resolveAuthConfigs(
      input({
        rows: [providerRow({ discovery_url: null, authorization_url: null, token_url: null })],
      }),
    );
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("provider_endpoints_incomplete");
  });

  test("an empty allow-list denies every claim, so it is refused up front", () => {
    const result = resolveAuthConfigs(input({ rows: [providerRow({ allowed_email_domains: [] })] }));
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("allowed_email_domains_empty");
  });

  test("THE B1 DEFECT, caught on the read path too", () => {
    // A row with no `trusted_jwt_issuer_id` can be signed into and can never
    // produce a successful claim: `admission_policy` matches neither stage, so
    // every claim is 403 admin_claim_domain_not_allowed. From 4B it is worse
    // still — the binding is where the minted `iss` comes from.
    const result = resolveAuthConfigs(
      input({ rows: [providerRow({ trusted_jwt_issuer_id: null })] }),
    );
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("provider_not_bound_to_trusted_jwt_issuer");
  });

  test("WAVE 4B: a binding to a trusted issuer that is not readable is refused", () => {
    const result = resolveAuthConfigs(input({ trustedIssuers: [] }));
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("trusted_jwt_issuer_not_resolvable");
  });

  test("WAVE 4B: a DISABLED trusted issuer is not a source of an issuer string", () => {
    // `resolve_active_issuer` refuses a disabled issuer at claim time with
    // 400 unregistered_trusted_issuer, so minting under it would defer the
    // failure to the last step.
    const result = resolveAuthConfigs(input({ trustedIssuers: [issuerRow({ status: "disabled" })] }));
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("trusted_jwt_issuer_not_resolvable");
  });

  test("WAVE 4B: an issuer string the console cannot parse gets no button", () => {
    const result = resolveAuthConfigs(
      input({ trustedIssuers: [issuerRow({ issuer: "https://elsewhere.test/idp/corp" })] }),
    );
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("trusted_jwt_issuer_not_resolvable");
  });

  test("D7 drift: Moira has the provider, the console has no secret", () => {
    const result = resolveAuthConfigs(input({ sealed: new Map(), secrets: new Map() }));
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("console_secret_unavailable");
    expect(result.drift).toBe("console_secret_missing");
  });

  test("D7 drift: the client id was rotated in Moira but not in the console", () => {
    const result = resolveAuthConfigs(
      input({ rows: [providerRow({ client_id: "rotated.apps.idp.test" })] }),
    );
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("console_secret_unavailable");
    expect(result.drift).toBe("client_id_mismatch");
  });

  test("the resolved config never carries a message key on success", () => {
    const result = resolveAuthConfigs(input());
    expect(result.ok).toBe(true);
    expect(JSON.stringify(result)).not.toContain("console.error");
  });
});

/* -------------------------------------------------------------------------- */
/* N providers (T7/T9) — the capability, resolved                             */
/* -------------------------------------------------------------------------- */

/** A second, GitHub-shaped provider on its own trusted issuer. */
function githubRow(): AuthProviderSettingsRecord {
  return providerRow({
    id: SECOND_PROVIDER_ROW_ID,
    method: "github_oauth",
    display_name: "GitHub",
    // `migrations/0020` requires both to be null on this method.
    issuer: null,
    discovery_url: null,
    authorization_url: "https://github.test/login/oauth/authorize",
    token_url: "https://github.test/login/oauth/access_token",
    userinfo_url: "https://api.github.test/user",
    allowed_email_domains: ["contractor.test"],
    trusted_jwt_issuer_id: SECOND_ISSUER_ROW_ID,
  });
}

function githubIssuerRow(): TrustedJwtIssuerRecord {
  return issuerRow({
    id: SECOND_ISSUER_ROW_ID,
    issuer: consoleIssuerForSlug(BFF_ISSUER, "github"),
  });
}

describe("two providers resolve independently, each with its own issuer", () => {
  const twoProviders = () =>
    input({
      rows: [providerRow(), githubRow()],
      trustedIssuers: [issuerRow(), githubIssuerRow()],
    });

  test("both resolve, and their minted issuers DIFFER", () => {
    const result = resolveAuthConfigs(twoProviders());
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.configs).toHaveLength(2);

    const issuers = result.configs.map((config) => config.consoleIssuer);
    // The F24 closure, at the resolution layer: `admin_identities` is keyed on
    // `(issuer, subject)`, so two providers sharing one issuer string means two
    // IdPs returning the same `sub` collapse onto one admin grant.
    expect(new Set(issuers).size).toBe(2);
    expect(issuers).toContain(BFF_ISSUER);
    expect(issuers).toContain(consoleIssuerForSlug(BFF_ISSUER, "github"));

    const ids = result.configs.map((config) => config.providerId);
    expect(new Set(ids).size).toBe(2);
    expect(ids).toContain(CONSOLE_OAUTH_PROVIDER_ID);
  });

  test("the incumbent's identity is untouched by the arrival of a second provider", () => {
    // The upgrade property in miniature: adding a provider must not renumber the
    // one every existing admin signed in through.
    const alone = resolveAuthConfigs(input());
    const together = resolveAuthConfigs(twoProviders());
    expect(alone.ok && together.ok).toBe(true);
    if (!alone.ok || !together.ok) return;
    const incumbentAlone = alone.configs[0]!;
    const incumbentTogether = together.configs.find(
      (config) => config.moiraProviderId === incumbentAlone.moiraProviderId,
    )!;
    expect(incumbentTogether.providerId).toBe(incumbentAlone.providerId);
    expect(incumbentTogether.consoleIssuer).toBe(incumbentAlone.consoleIssuer);
  });

  test("A DRIFTED SECOND PROVIDER DOES NOT TAKE THE FIRST ONE DOWN", () => {
    // The reason `ConsoleRuntime`'s failure union became per-provider. Before
    // 4B one bad row was the whole resolution, so a GitHub client secret that
    // had drifted out of the console's store would have removed OIDC sign-in
    // too — on a console whose only other way in is the bootstrap system key
    // the operator was told to remove.
    const base = twoProviders();
    const sealed = new Map(base.sealed);
    const secrets = new Map(base.secrets);
    sealed.delete(SECOND_PROVIDER_ROW_ID);
    secrets.delete(SECOND_PROVIDER_ROW_ID);

    const result = resolveAuthConfigs({ ...base, sealed, secrets });
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.configs).toHaveLength(1);
    expect(result.configs[0]!.providerId).toBe(CONSOLE_OAUTH_PROVIDER_ID);
    expect(result.problems).toHaveLength(1);
    expect(result.problems[0]!.moiraProviderId).toBe(SECOND_PROVIDER_ROW_ID);
    expect(result.problems[0]!.problem).toBe("console_secret_unavailable");
  });

  test("when NOTHING resolves the deployment-level failure is the first row's own reason", () => {
    // Not a generic "nothing works": on the overwhelmingly common
    // single-provider deployment the operator must still get the specific
    // remedy, with the same key this file emitted before 4B.
    const base = twoProviders();
    const result = resolveAuthConfigs({ ...base, sealed: new Map(), secrets: new Map() });
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("console_secret_unavailable");
  });

  test("ordering is by row id, not by list order", () => {
    const forwards = resolveAuthConfigs(twoProviders());
    const backwards = resolveAuthConfigs({
      ...twoProviders(),
      rows: [githubRow(), providerRow()],
    });
    expect(forwards.ok && backwards.ok).toBe(true);
    if (!forwards.ok || !backwards.ok) return;
    expect(forwards.configs.map((c) => c.moiraProviderId)).toEqual(
      backwards.configs.map((c) => c.moiraProviderId),
    );
  });
});

/* -------------------------------------------------------------------------- */
/* The ambiguity guard — T11, deliberately still standing                     */
/* -------------------------------------------------------------------------- */

describe("ambiguityGuard", () => {
  test("one enabled row passes through untouched", () => {
    const resolution = resolveAuthConfigs(input());
    expect(ambiguityGuard(1, resolution)).toBe(resolution);
  });

  test("two enabled rows are STILL refused, even though both resolved", () => {
    // This is the wave-4B state of play in one assertion: the machinery below it
    // resolves N providers correctly, and the console does not yet serve them.
    // The guard may only be removed once wave 4A's partial unique index and its
    // coded 409 are DEPLOYED — a merged migration is not a deployed one, and
    // this process cannot tell the difference.
    const resolution = resolveAuthConfigs(
      input({ rows: [providerRow(), githubRow()], trustedIssuers: [issuerRow(), githubIssuerRow()] }),
    );
    expect(resolution.ok).toBe(true);
    const guarded = ambiguityGuard(2, resolution);
    expect(guarded.ok).toBe(false);
    if (guarded.ok) return;
    expect(guarded.problem).toBe("ambiguous_enabled_providers");
  });

  test("the refusal keeps the cache key, so the caller does not rebuild on every read", () => {
    const resolution = resolveAuthConfigs(input());
    expect(ambiguityGuard(2, resolution).cacheKey).toBe(resolution.cacheKey);
  });
});

/* -------------------------------------------------------------------------- */
/* loadAuthConfigs — the Moira round trip                                     */
/* -------------------------------------------------------------------------- */

const listProviders = (rows: AuthProviderSettingsRecord[]) => () => ({
  status: 200,
  body: { data: rows, pagination: { has_more: false, next_cursor: null } },
});
const listIssuers = (rows: TrustedJwtIssuerRecord[]) => () => ({
  status: 200,
  body: { data: rows, pagination: { has_more: false, next_cursor: null } },
});

describe("loadAuthConfigs", () => {
  test("reads both lists and marries them to the console's own secret", async () => {
    const stub = createMoiraStub({
      "GET /api/v1/admin/auth/providers": listProviders([providerRow()]),
      "GET /api/v1/admin/jwt-issuers": listIssuers([issuerRow()]),
    });
    const client = new MoiraClient({
      baseUrl: MOIRA_STUB_BASE_URL,
      systemKey: "sk_test",
      fetch: stub.fetch,
    });
    const store = new InMemoryConsoleSecretStore(KEY);
    await store.put(providerRow().id, CLIENT_ID, SECRET);

    const result = await loadAuthConfigs(client, store, BFF_ISSUER);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.configs[0]!.clientSecret).toBe(SECRET);
    expect(result.configs[0]!.consoleIssuer).toBe(BFF_ISSUER);
    expect(stub.routes().sort()).toEqual([
      "GET /api/v1/admin/auth/providers",
      "GET /api/v1/admin/jwt-issuers",
    ]);
  });

  test("does not reveal a secret for a provider it is not going to use", async () => {
    // A disabled row's plaintext is never pulled out of the store on the way to
    // resolving the enabled one.
    let revealed: string[] = [];
    const stub = createMoiraStub({
      "GET /api/v1/admin/auth/providers": listProviders([
        providerRow(),
        providerRow({ id: SECOND_PROVIDER_ROW_ID, enabled: false }),
      ]),
      "GET /api/v1/admin/jwt-issuers": listIssuers([issuerRow()]),
    });
    const client = new MoiraClient({
      baseUrl: MOIRA_STUB_BASE_URL,
      systemKey: "sk_test",
      fetch: stub.fetch,
    });
    const store = new InMemoryConsoleSecretStore(KEY);
    await store.put(providerRow().id, CLIENT_ID, SECRET);
    await store.put(SECOND_PROVIDER_ROW_ID, CLIENT_ID, SECRET);
    const counting = {
      ...store,
      reveal: async (id: string) => {
        revealed.push(id);
        return store.reveal(id);
      },
      read: store.read.bind(store),
      put: store.put.bind(store),
      remove: store.remove.bind(store),
      newestUpdatedAt: store.newestUpdatedAt.bind(store),
    };

    const result = await loadAuthConfigs(client, counting, BFF_ISSUER);
    expect(result.ok).toBe(true);
    expect(revealed).toEqual([providerRow().id]);
  });

  test("the shipped path still refuses two enabled providers (T11)", async () => {
    const stub = createMoiraStub({
      "GET /api/v1/admin/auth/providers": listProviders([providerRow(), githubRow()]),
      "GET /api/v1/admin/jwt-issuers": listIssuers([issuerRow(), githubIssuerRow()]),
    });
    const client = new MoiraClient({
      baseUrl: MOIRA_STUB_BASE_URL,
      systemKey: "sk_test",
      fetch: stub.fetch,
    });
    const store = new InMemoryConsoleSecretStore(KEY);
    await store.put(providerRow().id, CLIENT_ID, SECRET);
    await store.put(SECOND_PROVIDER_ROW_ID, CLIENT_ID, SECRET);

    const result = await loadAuthConfigs(client, store, BFF_ISSUER);
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.problem).toBe("ambiguous_enabled_providers");
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
    expect(hasUsableEndpoints(providerRow({ authorization_url: null, token_url: null }))).toBe(
      true,
    );
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

  test("a GitHub row additionally needs a userinfo URL", () => {
    // GitHub issues no `id_token`, so the profile can only come from userinfo.
    // Without one, better-auth's `getUserInfo` returns null and the callback
    // fails `user_info_is_missing` several redirects in rather than here.
    expect(hasUsableEndpoints({ ...githubRow(), userinfo_url: null })).toBe(false);
    expect(hasUsableEndpoints(githubRow())).toBe(true);
  });

  test("github_oauth joins the interactive methods; jwks still does not", () => {
    expect(isInteractiveMethod("google_oauth")).toBe(true);
    expect(isInteractiveMethod("generic_oidc")).toBe(true);
    expect(isInteractiveMethod("github_oauth")).toBe(true);
    expect(isInteractiveMethod("jwks")).toBe(false);
  });
});
