// Wave 4B's three guards: G8, G9, G10 — on real PostgreSQL, through real
// sockets, against two providers that are genuinely different from each other.
//
// ============================================================================
// WHY THIS FILE IS NOT `oauth-flow.test.ts`
// ============================================================================
//
// That file drives ONE provider and asserts the token verifies. Every assertion
// in it stays green under the defect this wave exists to close, because with one
// provider there is nothing for `iss` to be wrong ABOUT. These three guards are
// the ones that can only fail when a second provider exists.
//
// ============================================================================
// WHY POSTGRESQL AND NOT THE MEMORY ADAPTER
// ============================================================================
//
// `memoryAdapter` stores rows as plain objects in an array: it will happily hold
// a property no schema declares, so "the provider stamp round-trips" is a much
// weaker claim there than it looks. On Kysely the column must exist, must be
// nullable, must be written by the create path and must be read back by the
// select path — four things the memory adapter proves none of. G9 additionally
// needs rows INSERTED BY RAW SQL, which is the only way to build a pre-upgrade
// fixture that the new code did not generate.
//
// ============================================================================
// THE TOOTHLESSNESS CHECK, WRITTEN DOWN RATHER THAN ASSUMED
// ============================================================================
//
// In stage 4A, guard G1's specified mutation left G1 GREEN: `migrations/0020`
// had made the defect it targeted unrepresentable, so a fixture built from legal
// rows could no longer reach it. Each guard below therefore records (a) the
// mutation that must turn it red, and (b) whether the fixture can still
// REPRESENT the broken state once that mutation is applied. Both were applied by
// hand and observed; see `plans/reports/EXECUTION-LEDGER.md`.

import { afterAll, beforeAll, expect, test, describe } from "bun:test";
import { createRemoteJWKSet, decodeProtectedHeader, jwtVerify } from "jose";
import type { Pool } from "pg";

import {
  MissingIdpSubjectError,
  MOIRA_JWT_ALGORITHM,
  readIdpSubject,
  SESSION_PROVIDER_FIELD,
  type ConsoleAuthContext,
} from "@/lib/auth";
import {
  resolveAuthConfigs,
  type AuthConfigsInput,
  type ResolvedAuthConfig,
} from "@/lib/auth-config";
import { sealClientSecret } from "@/lib/console-secrets";
import { readConsoleEnv, type ConsoleEnv } from "@/lib/env";
import { SESSION_REJECTION_MESSAGE_KEYS } from "@/lib/moira-session";
import type { AuthProviderSettingsRecord, TrustedJwtIssuerRecord } from "@/lib/types";

import { createBrowserAgent, type BrowserAgent } from "../support/browser-agent";
import {
  DATABASE_TESTS_SKIPPED,
  describeDatabase,
  openConsoleTestDatabase,
  resetConsoleTestDatabase,
} from "../support/console-db";
import { reserveConsolePort, startConsoleServer, type ConsoleServer } from "../support/console-server";
import { trustFixtureCa, untrustFixtureCa } from "../support/fixture-tls";
import { startMockGithub, type MockGithub } from "../support/mock-github";
import { startMockIdp, type MockIdp } from "../support/mock-idp";
import { restoreDomWhatwgGlobals, useNativeWhatwgGlobals } from "../support/native-globals";

/* -------------------------------------------------------------------------- */
/* Fixture literals — deliberately literals                                   */
/* -------------------------------------------------------------------------- */

/**
 * The console's own issuer. FIXED, not derived from the ephemeral test port, so
 * that G9 can assert against a genuine literal rather than against a value the
 * code under test computed.
 */
const BFF_ISSUER = "https://console.w4b.test";

/**
 * The two strings a pre-4B deployment already has on disk.
 *
 * WRITTEN OUT, not imported from `lib/auth-config.ts`. A fixture built from the
 * new code's helpers would generate whatever the new scheme generates and could
 * not catch the thing G9 exists to catch. `CONSOLE_OAUTH_PROVIDER_ID` is
 * deliberately NOT imported into this file.
 */
const PRE_4B_PROVIDER_ID = "moira-console-idp";
const PRE_4B_ISSUER = "https://console.w4b.test";

/** The additional provider's issuer, also spelled out rather than derived. */
const GITHUB_CONSOLE_ISSUER = "https://console.w4b.test/idp/github";
const GITHUB_PROVIDER_ID = "moira-console-idp-github";

const AUDIENCE = "moira-admin-api";
const SECRET_KEY = Buffer.alloc(32, 0x5b);

const OIDC_ROW_ID = "11111111-1111-4111-8111-111111111111";
const GITHUB_ROW_ID = "33333333-3333-4333-8333-333333333333";
const OIDC_ISSUER_ROW_ID = "22222222-2222-4222-8222-222222222222";
const GITHUB_ISSUER_ROW_ID = "44444444-4444-4444-8444-444444444444";

const OIDC_CLIENT_ID = "console.apps.mock-idp.test";
const OIDC_CLIENT_SECRET = "mock-idp-client-secret-do-not-reuse";
const GITHUB_CLIENT_ID = "Iv1.mockgithubclient";
const GITHUB_CLIENT_SECRET = "mock-github-client-secret-do-not-reuse";

/* -------------------------------------------------------------------------- */
/* Helpers                                                                    */
/* -------------------------------------------------------------------------- */

function envFor(consoleOrigin: string): ConsoleEnv {
  return readConsoleEnv({
    NODE_ENV: "test",
    MOIRA_API_URL: "https://moira.invalid",
    CONSOLE_PUBLIC_ORIGIN: consoleOrigin,
    // Pinned so `iss` does not move with the ephemeral port.
    MOIRA_BFF_ISSUER_URL: BFF_ISSUER,
    MOIRA_ADMIN_API_AUDIENCE: AUDIENCE,
    BETTER_AUTH_SECRET: "fixture-better-auth-secret-at-least-32-chars",
    CONSOLE_SECRET_ENCRYPTION_KEY: SECRET_KEY.toString("base64"),
  });
}

function oidcRow(idp: MockIdp, overrides: Partial<AuthProviderSettingsRecord> = {}) {
  return {
    id: OIDC_ROW_ID,
    method: "generic_oidc",
    display_name: "Corporate IdP",
    enabled: true,
    requested_scopes: ["openid", "email", "profile"],
    allowed_email_domains: ["corp.test"],
    allowed_algorithms: ["ES256"],
    expected_audiences: [],
    redirect_uris: [],
    metadata: null,
    status: "active",
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    version: 1,
    issuer: idp.issuer,
    discovery_url: idp.discoveryUrl,
    client_id: OIDC_CLIENT_ID,
    trusted_jwt_issuer_id: OIDC_ISSUER_ROW_ID,
    ...overrides,
  } satisfies AuthProviderSettingsRecord;
}

function githubRow(github: MockGithub, overrides: Partial<AuthProviderSettingsRecord> = {}) {
  return {
    id: GITHUB_ROW_ID,
    method: "github_oauth",
    display_name: "GitHub",
    enabled: true,
    requested_scopes: ["read:user", "user:email"],
    allowed_email_domains: ["contractor.test"],
    allowed_algorithms: ["ES256"],
    expected_audiences: [],
    redirect_uris: [],
    metadata: null,
    status: "active",
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    version: 1,
    // `migrations/0020` requires BOTH to be null on this method: GitHub is not
    // OIDC and has neither.
    issuer: null,
    discovery_url: null,
    authorization_url: github.authorizationUrl,
    token_url: github.tokenUrl,
    userinfo_url: github.userInfoUrl,
    client_id: GITHUB_CLIENT_ID,
    trusted_jwt_issuer_id: GITHUB_ISSUER_ROW_ID,
    ...overrides,
  } satisfies AuthProviderSettingsRecord;
}

function issuerRow(id: string, issuer: string): TrustedJwtIssuerRecord {
  return {
    id,
    issuer,
    jwks_url: `${BFF_ISSUER}/api/auth/.well-known/jwks.json`,
    expected_audiences: [AUDIENCE],
    allowed_algorithms: ["ES256"],
    subject_claim: "sub",
    clock_skew_seconds: 60,
    allow_delegation: false,
    status: "active",
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    version: 1,
  };
}

/**
 * Resolve through the SHIPPED code path.
 *
 * The configurations under test are produced by `resolveAuthConfigs` rather than
 * hand-written, so a change to how `providerId` or `consoleIssuer` is derived is
 * visible here. The fixture ROWS are hand-written; the derivation is not.
 */
function resolveFixture(
  rows: readonly AuthProviderSettingsRecord[],
  issuers: readonly TrustedJwtIssuerRecord[],
  secrets: ReadonlyMap<string, string>,
): readonly ResolvedAuthConfig[] {
  const input: AuthConfigsInput = {
    rows,
    trustedIssuers: issuers,
    bffIssuerUrl: BFF_ISSUER,
    sealed: new Map(
      rows.map((row) => [
        row.id,
        sealClientSecret(SECRET_KEY, row.id, row.client_id ?? "", secrets.get(row.id) ?? ""),
      ]),
    ),
    secrets: new Map(rows.map((row) => [row.id, secrets.get(row.id) ?? null])),
    newestSecretUpdatedAt: null,
  };
  const resolution = resolveAuthConfigs(input);
  if (!resolution.ok) {
    throw new Error(
      `the multi-provider fixture did not resolve: ${resolution.problem}. ` +
        "Every guard below would then pass on an empty world.",
    );
  }
  // Deliberately NOT through `loadAuthConfigs`: `ambiguityGuard` still refuses a
  // deployment with more than one enabled row until wave 4A is DEPLOYED (T11),
  // and these guards are about the minting machinery underneath that gate.
  return resolution.configs;
}

async function signIn(origin: string, providerId: string): Promise<BrowserAgent> {
  const agent = createBrowserAgent();
  const start = await agent.request(`${origin}/api/auth/sign-in/oauth2`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ providerId, callbackURL: "/" }),
  });
  expect(start.status, `sign-in did not start for ${providerId}`).toBe(200);
  const { url } = (await start.json()) as { url: string };
  await agent.navigate(url);
  return agent;
}

function oauthErrorOf(agent: BrowserAgent): string | null {
  for (const hop of agent.hops) {
    if (hop.location === null) continue;
    const error = new URL(hop.location, hop.url).searchParams.get("error");
    if (error !== null) return error;
  }
  return null;
}

async function mint(
  agent: BrowserAgent,
  origin: string,
): Promise<{ status: number; token: string | null; code: string | null }> {
  const response = await agent.request(`${origin}/api/auth/token`);
  const body = (await response.json().catch(() => null)) as
    | { token?: unknown; error?: { code?: unknown } }
    | null;
  return {
    status: response.status,
    token: typeof body?.token === "string" ? body.token : null,
    code: typeof body?.error?.code === "string" ? body.error.code : null,
  };
}

/* ========================================================================== */
/* G8 — the token's `iss` names the provider that authenticated the session   */
/* ========================================================================== */

/**
 * MUTATION THAT MUST TURN THIS RED: make `jwt.definePayload` in `lib/auth.ts`
 * omit `iss`, so `sign.mjs`'s `.setIssuer(iss ?? defaultIss)` falls back to
 * `options.jwt.issuer` — which the console sets to `env.bffIssuerUrl`.
 *
 * CAN THE FIXTURE STILL REPRESENT THE BROKEN STATE? Yes. Nothing in wave 4
 * constrains what `definePayload` may return, so the mutated arrangement is
 * fully representable and produces two tokens that both carry the incumbent's
 * issuer. The assertion that catches it is `issA !== issB`.
 *
 * THE TOOTHLESS VERSION, REJECTED: "the token verifies against the JWKS". It
 * does in BOTH arrangements — there is one ES256 key pair and one `kid`, and
 * `iss` is not part of the signature. So is "the incumbent's token carries
 * `bffIssuerUrl`", because that is exactly what the fallback produces. The
 * load-bearing assertions are that the two issuers DIFFER and that the
 * non-incumbent one equals its own trusted-issuer string.
 */
describeDatabase("G8 — the minted `iss` names the authenticating provider", () => {
  let pool: Pool;
  let idp: MockIdp;
  let github: MockGithub;
  let server: ConsoleServer;
  let configs: readonly ResolvedAuthConfig[];

  beforeAll(async () => {
    if (DATABASE_TESTS_SKIPPED) return;
    useNativeWhatwgGlobals();
    pool = await openConsoleTestDatabase();
    await resetConsoleTestDatabase(pool);

    const port = reserveConsolePort();
    const origin = `https://localhost:${port}`;
    trustFixtureCa(origin);

    idp = await startMockIdp({
      clientId: OIDC_CLIENT_ID,
      clientSecret: OIDC_CLIENT_SECRET,
      user: {
        sub: "corp-idp-subject-g8",
        email: "operator@corp.test",
        emailVerified: true,
        name: "Corp Operator",
      },
    });
    github = await startMockGithub({
      clientId: GITHUB_CLIENT_ID,
      clientSecret: GITHUB_CLIENT_SECRET,
      user: {
        // A short numeric id — the shape that makes F24 concrete, since a
        // generic-OIDC IdP returning a numeric `sub` collides with it.
        id: 4242,
        login: "contractor",
        name: "Contractor Person",
        email: "person@contractor.test",
      },
    });
    trustFixtureCa(idp.origin);
    trustFixtureCa(github.origin);

    configs = resolveFixture(
      [oidcRow(idp), githubRow(github)],
      [
        issuerRow(OIDC_ISSUER_ROW_ID, PRE_4B_ISSUER),
        issuerRow(GITHUB_ISSUER_ROW_ID, GITHUB_CONSOLE_ISSUER),
      ],
      new Map([
        [OIDC_ROW_ID, OIDC_CLIENT_SECRET],
        [GITHUB_ROW_ID, GITHUB_CLIENT_SECRET],
      ]),
    );

    server = startConsoleServer({ env: envFor(origin), configs, database: pool }, port);
  });

  afterAll(async () => {
    if (DATABASE_TESTS_SKIPPED) return;
    server?.stop();
    idp?.stop();
    github?.stop();
    untrustFixtureCa();
    restoreDomWhatwgGlobals();
    await pool?.end();
  });

  test("the fixture really is two DIFFERENT providers, not one shape twice", () => {
    // The floor. Without it a copy-paste that pointed both entries at the OIDC
    // mock would leave every assertion below green while testing nothing about
    // GitHub.
    expect(configs).toHaveLength(2);
    expect(configs.map((c) => c.method).sort()).toEqual(["generic_oidc", "github_oauth"]);
    expect(configs.map((c) => c.providerId).sort()).toEqual(
      [PRE_4B_PROVIDER_ID, GITHUB_PROVIDER_ID].sort(),
    );
    expect(new Set(configs.map((c) => c.consoleIssuer)).size).toBe(2);
  });

  test("two flows, two tokens, and the two `iss` values DIFFER", async () => {
    const oidcAgent = await signIn(server.origin, PRE_4B_PROVIDER_ID);
    expect(oauthErrorOf(oidcAgent)).toBeNull();
    const githubAgent = await signIn(server.origin, GITHUB_PROVIDER_ID);
    expect(oauthErrorOf(githubAgent)).toBeNull();

    // Both providers actually served their own flow.
    expect(idp.routes()).toContain("POST /token");
    expect(github.routes()).toContain("POST /login/oauth/access_token");
    // And GitHub's non-OIDC shape was exercised, not an OIDC one wearing its
    // name: no discovery document was ever fetched from it, and the profile came
    // from the two REST endpoints.
    expect(github.routes()).not.toContain("GET /.well-known/openid-configuration");
    expect(github.routes()).toContain("GET /user");
    expect(github.routes()).toContain("GET /user/emails");

    const jwks = createRemoteJWKSet(new URL(server.jwksUrl));

    const oidcToken = await mint(oidcAgent, server.origin);
    expect(oidcToken.status).toBe(200);
    const githubToken = await mint(githubAgent, server.origin);
    expect(githubToken.status).toBe(200);

    const oidcVerified = await jwtVerify(String(oidcToken.token), jwks, {
      issuer: PRE_4B_ISSUER,
      audience: AUDIENCE,
      algorithms: [MOIRA_JWT_ALGORITHM],
    });
    const githubVerified = await jwtVerify(String(githubToken.token), jwks, {
      issuer: GITHUB_CONSOLE_ISSUER,
      audience: AUDIENCE,
      algorithms: [MOIRA_JWT_ALGORITHM],
    });

    // THE ASSERTION WITH TEETH. Under a `definePayload` that omits `iss`, both
    // of these are `BFF_ISSUER` and this line is the one that goes red.
    expect(
      oidcVerified.payload.iss,
      "both tokens carry the same issuer: `definePayload` is not supplying one, so " +
        "`options.jwt.issuer` is being used for every provider. That is finding F24 — " +
        "`admin_identities` is keyed on (issuer, subject), so two IdPs returning the same " +
        "`sub` resolve to ONE admin grant.",
    ).not.toBe(githubVerified.payload.iss);

    // And each is its own trusted issuer's registered string, compared against
    // the literals at the top of this file rather than against `config.*`.
    expect(oidcVerified.payload.iss).toBe(PRE_4B_ISSUER);
    expect(githubVerified.payload.iss).toBe(GITHUB_CONSOLE_ISSUER);

    // The subjects are each provider's own. GitHub's numeric id arrives as text.
    expect(oidcVerified.payload.sub).toBe("corp-idp-subject-g8");
    expect(githubVerified.payload.sub).toBe("4242");

    // One key pair, one `kid`, N issuer strings — stated as an assertion so the
    // absence of cryptographic separation is recorded rather than assumed. This
    // is why `iss` selection is the whole security boundary.
    expect(decodeProtectedHeader(String(oidcToken.token)).kid).toBe(
      decodeProtectedHeader(String(githubToken.token)).kid,
    );
  });

  test("the session row records which provider authenticated it", async () => {
    const rows = await pool.query<{ providerId: string | null }>(
      `select "${SESSION_PROVIDER_FIELD}" from "session" order by "createdAt"`,
    );
    // Both stamps present in the actual column, on a real adapter.
    expect(rows.rows.map((row) => row.providerId)).toContain(PRE_4B_PROVIDER_ID);
    expect(rows.rows.map((row) => row.providerId)).toContain(GITHUB_PROVIDER_ID);
  });

  test("a session that predates the column REFUSES to mint, and does not default", async () => {
    // Constraint 1 and 2 from the T0 spike, as shipped coverage. `0003` adds the
    // column nullable, so every session live at deploy time looks exactly like
    // this — and a mint that defaulted would authorise it against a provider
    // that did not authenticate it.
    const agent = await signIn(server.origin, GITHUB_PROVIDER_ID);
    expect((await mint(agent, server.origin)).status).toBe(200);

    const nulled = await pool.query(
      `update "session" set "${SESSION_PROVIDER_FIELD}" = null where "${SESSION_PROVIDER_FIELD}" = $1`,
      [GITHUB_PROVIDER_ID],
    );
    expect(nulled.rowCount).toBeGreaterThan(0);

    const after = await mint(agent, server.origin);
    expect(after.status).not.toBe(200);
    expect(after.token).toBeNull();
    expect(after.code).toBe("session_provider_unknown");
  });

  test("...and an ordinary session READ still answers 200 for that same session", async () => {
    // Constraint 3, which the design did not anticipate: the jwt plugin also
    // mints on `/get-session` (an `after` hook setting `set-auth-jwt`), so a
    // refusal that throws would turn every page load of an un-upgraded session
    // into a 500. `disableSettingJwtHeader: true` is the fix, and this is the
    // assertion that would go red if it were removed.
    const agent = await signIn(server.origin, GITHUB_PROVIDER_ID);
    await pool.query(`update "session" set "${SESSION_PROVIDER_FIELD}" = null`);
    const session = await agent.request(`${server.origin}/api/auth/get-session`);
    expect(
      session.status,
      "a session read 500s for an un-upgraded session: the jwt plugin is still minting on " +
        "/get-session, so the refusal escapes as an unhandled throw",
    ).toBe(200);
    expect((await mint(agent, server.origin)).status).not.toBe(200);
  });
});

/* ========================================================================== */
/* GitHub's shape, exercised for what makes it different (T10)                */
/* ========================================================================== */

describeDatabase("GitHub's non-OIDC profile is read safely", () => {
  let pool: Pool;
  let github: MockGithub;
  let server: ConsoleServer;

  async function startWith(
    options: Parameters<typeof startMockGithub>[0],
    allowedEmailDomains: string[] = ["contractor.test"],
  ): Promise<void> {
    const port = reserveConsolePort();
    const origin = `https://localhost:${port}`;
    trustFixtureCa(origin);
    github = await startMockGithub(options);
    trustFixtureCa(github.origin);
    const configs = resolveFixture(
      [githubRow(github, { allowed_email_domains: allowedEmailDomains })],
      [issuerRow(GITHUB_ISSUER_ROW_ID, GITHUB_CONSOLE_ISSUER)],
      new Map([[GITHUB_ROW_ID, GITHUB_CLIENT_SECRET]]),
    );
    server = startConsoleServer({ env: envFor(origin), configs, database: pool }, port);
  }

  beforeAll(async () => {
    if (DATABASE_TESTS_SKIPPED) return;
    useNativeWhatwgGlobals();
    pool = await openConsoleTestDatabase();
  });

  afterAll(async () => {
    if (DATABASE_TESTS_SKIPPED) return;
    untrustFixtureCa();
    restoreDomWhatwgGlobals();
    await pool?.end();
  });

  test("the VERIFIED primary address wins over the public profile address", async () => {
    await resetConsoleTestDatabase(pool);
    await startWith({
      clientId: GITHUB_CLIENT_ID,
      clientSecret: GITHUB_CLIENT_SECRET,
      user: {
        id: 909,
        login: "person",
        name: "Person",
        email: "person@contractor.test",
      },
      // The takeover shape: a GitHub account can set its PUBLIC profile email to
      // any string, including one an existing console admin already holds.
      publicProfileEmail: "admin@corp.test",
    });
    try {
      const agent = await signIn(server.origin, GITHUB_PROVIDER_ID);
      expect(oauthErrorOf(agent)).toBeNull();
      const users = await pool.query<{ email: string }>('select email from "user"');
      expect(
        users.rows.map((row) => row.email),
        "the console took `GET /user`'s public profile address. That address is " +
          "attacker-chosen, and implicit account linking would have linked it onto whichever " +
          "console user already holds it.",
      ).toEqual(["person@contractor.test"]);
    } finally {
      server?.stop();
      github?.stop();
    }
  });

  test("an UNVERIFIED primary address cannot produce a session at all", async () => {
    await resetConsoleTestDatabase(pool);
    await startWith({
      clientId: GITHUB_CLIENT_ID,
      clientSecret: GITHUB_CLIENT_SECRET,
      user: { id: 910, login: "person", name: "Person", email: "person@contractor.test" },
      emailsUnverified: true,
    });
    try {
      const agent = await signIn(server.origin, GITHUB_PROVIDER_ID);
      expect(oauthErrorOf(agent)).toBe("email_is_missing");
      expect(agent.cookieNames().join(",")).not.toContain("session_token");
      // Nothing was written: the refusal happens before any row exists.
      const users = await pool.query('select id from "user"');
      expect(users.rowCount).toBe(0);
    } finally {
      server?.stop();
      github?.stop();
    }
  });

  test("no `user:email` scope means no sign-in, not a fallback to the profile address", async () => {
    await resetConsoleTestDatabase(pool);
    await startWith({
      clientId: GITHUB_CLIENT_ID,
      clientSecret: GITHUB_CLIENT_SECRET,
      user: { id: 911, login: "person", name: "Person", email: "person@contractor.test" },
      emailsScopeDenied: true,
    });
    try {
      const agent = await signIn(server.origin, GITHUB_PROVIDER_ID);
      expect(oauthErrorOf(agent)).toBe("email_is_missing");
      expect(agent.cookieNames().join(",")).not.toContain("session_token");
    } finally {
      server?.stop();
      github?.stop();
    }
  });

  test("a GitHub identity outside the allow-list is refused at the CREDENTIAL, not the door", async () => {
    await resetConsoleTestDatabase(pool);
    await startWith(
      {
        clientId: GITHUB_CLIENT_ID,
        clientSecret: GITHUB_CLIENT_SECRET,
        user: { id: 912, login: "person", name: "Person", email: "person@contractor.test" },
      },
      // The provider's own list, which is now per provider: each has its own
      // trusted issuer row and therefore its own `admission_policy` lookup.
      ["someone-else.test"],
    );
    try {
      const agent = await signIn(server.origin, GITHUB_PROVIDER_ID);
      expect(oauthErrorOf(agent)).toBeNull();
      expect(agent.cookieNames().join(",")).toContain("session_token");
      const refusal = await mint(agent, server.origin);
      expect(refusal.status).toBe(403);
      expect(refusal.token).toBeNull();
      expect(refusal.code).toBe("email_domain_not_allowed");
      expect(SESSION_REJECTION_MESSAGE_KEYS.email_domain_not_allowed).toBeString();
    } finally {
      server?.stop();
      github?.stop();
    }
  });
});

/* ========================================================================== */
/* G9 — the upgrade does not orphan existing admins                           */
/* ========================================================================== */

/**
 * MUTATIONS THAT MUST TURN THIS RED, both spellings of "apply the derived scheme
 * to the legacy row":
 *
 *   (a) `consoleProviderIdFor` drops its incumbent branch, so the provider bound
 *       to `bffIssuerUrl` gets a derived id instead of `moira-console-idp`;
 *   (b) `resolveOne` DERIVES `consoleIssuer` from the provider row id instead of
 *       READING it from the bound trusted-issuer row — the UUID scheme the wave-4
 *       design named as the footgun.
 *
 * CAN THE FIXTURE STILL REPRESENT THE BROKEN STATE? Yes, and this is why the
 * fixture is raw SQL. The `user` and `account` rows below are INSERTED
 * DIRECTLY with the pre-4B literals; nothing about them is produced by the code
 * under test, so both mutations leave the fixture intact and merely make the
 * running code disagree with it. A fixture that had signed a user in through the
 * new code first would have written whatever the new scheme generates and would
 * have agreed with itself under either mutation.
 *
 * WHAT IS *NOT* ASSERTED HERE, deliberately: the minted `sub` alone. Under
 * mutation (a) the sign-in below would create a SECOND `account` row under the
 * derived id carrying the same IdP subject, so `sub` would still be right while
 * every pre-existing grant was orphaned. The assertions with teeth are the two
 * frozen literals and the lookup under the old `providerId`.
 */
describeDatabase("G9 — a pre-4B admin is not orphaned by the upgrade", () => {
  let pool: Pool;
  let idp: MockIdp;
  let server: ConsoleServer;
  let configs: readonly ResolvedAuthConfig[];

  /** The subject a pre-4B `admin_identities` grant was created against. */
  const LEGACY_SUBJECT = "corp-idp-subject-legacy-0f3a91";
  const LEGACY_EMAIL = "incumbent@corp.test";
  const LEGACY_USER_ID = "pre4b-user-row";

  beforeAll(async () => {
    if (DATABASE_TESTS_SKIPPED) return;
    useNativeWhatwgGlobals();
    pool = await openConsoleTestDatabase();
    await resetConsoleTestDatabase(pool);

    const port = reserveConsolePort();
    const origin = `https://localhost:${port}`;
    trustFixtureCa(origin);

    idp = await startMockIdp({
      clientId: OIDC_CLIENT_ID,
      clientSecret: OIDC_CLIENT_SECRET,
      user: {
        sub: LEGACY_SUBJECT,
        email: LEGACY_EMAIL,
        emailVerified: true,
        name: "Incumbent Admin",
      },
    });
    trustFixtureCa(idp.origin);

    // ---- THE PRE-UPGRADE FIXTURE, WRITTEN BY HAND -------------------------
    // Exactly the rows a console that has been running since wave 3 has: a
    // `user`, an `account` under the literal `moira-console-idp`, and NO
    // `providerId` on any session (the column did not exist).
    await pool.query(
      `insert into "user" (id, name, email, "emailVerified", "createdAt", "updatedAt")
       values ($1, $2, $3, true, now(), now())`,
      [LEGACY_USER_ID, "Incumbent Admin", LEGACY_EMAIL],
    );
    await pool.query(
      `insert into "account" (id, "accountId", "providerId", "userId", "createdAt", "updatedAt")
       values ($1, $2, $3, $4, now(), now())`,
      ["pre4b-account-row", LEGACY_SUBJECT, PRE_4B_PROVIDER_ID, LEGACY_USER_ID],
    );

    // The 4B configuration this deployment resolves to AFTER the upgrade. The
    // provider row is bound to a trusted issuer whose string is the console's own
    // `bffIssuerUrl`, which is the definition of "the incumbent".
    configs = resolveFixture(
      [oidcRow(idp, { allowed_email_domains: ["corp.test"] })],
      [issuerRow(OIDC_ISSUER_ROW_ID, PRE_4B_ISSUER)],
      new Map([[OIDC_ROW_ID, OIDC_CLIENT_SECRET]]),
    );
    server = startConsoleServer({ env: envFor(origin), configs, database: pool }, port);
  });

  afterAll(async () => {
    if (DATABASE_TESTS_SKIPPED) return;
    server?.stop();
    idp?.stop();
    untrustFixtureCa();
    restoreDomWhatwgGlobals();
    await pool?.end();
  });

  test("the fixture is the pre-4B one — one account, under the OLD provider id", async () => {
    const accounts = await pool.query<{ providerId: string; accountId: string }>(
      'select "providerId", "accountId" from "account"',
    );
    expect(accounts.rows).toEqual([
      { providerId: PRE_4B_PROVIDER_ID, accountId: LEGACY_SUBJECT },
    ]);
  });

  test("the incumbent still resolves to the FROZEN provider id and issuer", () => {
    expect(configs).toHaveLength(1);
    const incumbent = configs[0]!;
    // Compared against literals, not against `CONSOLE_OAUTH_PROVIDER_ID`: this
    // string is in every `account` row on every existing deployment and in the
    // redirect URI registered by hand at the IdP. It is frozen, and a test that
    // imported the constant would move with it.
    expect(
      incumbent.providerId,
      "the incumbent's Better Auth provider id changed. Every existing `account` row holds " +
        "the old one, so `readIdpSubject` finds nothing and no admin can mint again; the " +
        "redirect URI registered at the IdP is also now wrong, which no console-side change " +
        "can repair.",
    ).toBe(PRE_4B_PROVIDER_ID);
    expect(
      incumbent.consoleIssuer,
      "the incumbent's minted `iss` changed. `admin_identities` is keyed on (issuer, subject), " +
        "so every grant made before the upgrade now matches nothing and every existing admin " +
        "is silently revoked.",
    ).toBe(PRE_4B_ISSUER);
  });

  test("`readIdpSubject` still finds the pre-existing account", async () => {
    const context = await (
      server.auth as unknown as { $context: Promise<ConsoleAuthContext> }
    ).$context;
    // Looked up under the OLD literal, which is what the row actually holds.
    await expect(readIdpSubject(context, PRE_4B_PROVIDER_ID, LEGACY_USER_ID)).resolves.toBe(
      LEGACY_SUBJECT,
    );
    // And the negative control: a derived id finds nothing, which is precisely
    // what the deployment would look like under the mutation.
    await expect(
      readIdpSubject(context, `${PRE_4B_PROVIDER_ID}-corp`, LEGACY_USER_ID),
    ).rejects.toBeInstanceOf(MissingIdpSubjectError);
  });

  test("signing in again mints the SAME (iss, sub) pair the old grant was made against", async () => {
    const agent = await signIn(server.origin, PRE_4B_PROVIDER_ID);
    expect(oauthErrorOf(agent)).toBeNull();

    // The sign-in adopted the pre-existing rows rather than creating new ones —
    // the premise that makes the rest of this test about the upgrade at all.
    const users = await pool.query<{ id: string }>('select id from "user"');
    expect(users.rows.map((row) => row.id)).toEqual([LEGACY_USER_ID]);
    const accounts = await pool.query<{ providerId: string }>('select "providerId" from "account"');
    expect(accounts.rows.map((row) => row.providerId)).toEqual([PRE_4B_PROVIDER_ID]);

    const minted = await mint(agent, server.origin);
    expect(minted.status).toBe(200);
    const { payload } = await jwtVerify(
      String(minted.token),
      createRemoteJWKSet(new URL(server.jwksUrl)),
      { issuer: PRE_4B_ISSUER, audience: AUDIENCE, algorithms: [MOIRA_JWT_ALGORITHM] },
    );
    // The pair Moira's `authenticate_admin` looks up in `admin_identities`
    // (`admin_identities_issuer_subject_active_unique` is on `(issuer, subject)`
    // where `deleted_at is null`). Both halves are the pre-4B literals.
    expect(payload.iss).toBe(PRE_4B_ISSUER);
    expect(payload.sub).toBe(LEGACY_SUBJECT);
  });
});

/* ========================================================================== */
/* G10 — the minted `sub` and `iss` come from the SAME account                */
/* ========================================================================== */

/**
 * MUTATION THAT MUST TURN THIS RED: make `readIdpSubject` return the first
 * account on the user (`accounts[0]`) rather than the one matching the resolved
 * `providerId` — the "most recently updated account" heuristic in its simplest
 * form, and the substitution the wave-4 design explicitly forbade.
 *
 * CAN THE FIXTURE STILL REPRESENT THE BROKEN STATE? Yes, and only because there
 * are TWO linked accounts. A single-account fixture passes in both arrangements:
 * `accounts[0]` and "the account for this provider" are the same row. That is
 * the toothless version, and it is why this block signs in through A first and
 * asserts `sub !== SUB_A` explicitly rather than only `sub === SUB_B`.
 *
 * The linking itself is not simulated: two mock IdPs return the SAME verified
 * email with DIFFERENT subjects, and Better Auth's implicit account linking
 * (`accountLinking.enabled` defaults on, `requireLocalEmailVerified` defaults
 * true, both sides verified) does the rest. One human, two grants, is the
 * consequence wave 4 ships and does not otherwise record anywhere executable.
 */
describeDatabase("G10 — `sub` and `iss` name the same account", () => {
  let pool: Pool;
  let idpA: MockIdp;
  let idpB: MockIdp;
  let server: ConsoleServer;

  const SHARED_EMAIL = "dual@corp.test";
  const SUB_A = "corp-idp-subject-aaaa";
  const SUB_B = "contractor-idp-subject-bbbb";
  const PROVIDER_B_ID = "moira-console-idp-contractors";
  const PROVIDER_B_ISSUER = "https://console.w4b.test/idp/contractors";
  const ROW_B_ID = "55555555-5555-4555-8555-555555555555";
  const ISSUER_B_ROW_ID = "66666666-6666-4666-8666-666666666666";
  const CLIENT_B_ID = "console-b.apps.mock-idp.test";
  const CLIENT_B_SECRET = "mock-idp-b-client-secret-do-not-reuse";

  beforeAll(async () => {
    if (DATABASE_TESTS_SKIPPED) return;
    useNativeWhatwgGlobals();
    pool = await openConsoleTestDatabase();
    await resetConsoleTestDatabase(pool);

    const port = reserveConsolePort();
    const origin = `https://localhost:${port}`;
    trustFixtureCa(origin);

    idpA = await startMockIdp({
      clientId: OIDC_CLIENT_ID,
      clientSecret: OIDC_CLIENT_SECRET,
      user: { sub: SUB_A, email: SHARED_EMAIL, emailVerified: true, name: "Dual Identity" },
    });
    idpB = await startMockIdp({
      clientId: CLIENT_B_ID,
      clientSecret: CLIENT_B_SECRET,
      user: { sub: SUB_B, email: SHARED_EMAIL, emailVerified: true, name: "Dual Identity" },
    });
    trustFixtureCa(idpA.origin);
    trustFixtureCa(idpB.origin);

    const configs = resolveFixture(
      [
        oidcRow(idpA),
        oidcRow(idpB, {
          id: ROW_B_ID,
          display_name: "Contractor IdP",
          issuer: idpB.issuer,
          discovery_url: idpB.discoveryUrl,
          client_id: CLIENT_B_ID,
          trusted_jwt_issuer_id: ISSUER_B_ROW_ID,
        }),
      ],
      [
        issuerRow(OIDC_ISSUER_ROW_ID, PRE_4B_ISSUER),
        issuerRow(ISSUER_B_ROW_ID, PROVIDER_B_ISSUER),
      ],
      new Map([
        [OIDC_ROW_ID, OIDC_CLIENT_SECRET],
        [ROW_B_ID, CLIENT_B_SECRET],
      ]),
    );
    server = startConsoleServer({ env: envFor(origin), configs, database: pool }, port);
  });

  afterAll(async () => {
    if (DATABASE_TESTS_SKIPPED) return;
    server?.stop();
    idpA?.stop();
    idpB?.stop();
    untrustFixtureCa();
    restoreDomWhatwgGlobals();
    await pool?.end();
  });

  test("one human, two linked accounts, two sessions — asserted in SQL", async () => {
    const agentA = await signIn(server.origin, PRE_4B_PROVIDER_ID);
    expect(oauthErrorOf(agentA)).toBeNull();
    const agentB = await signIn(server.origin, PROVIDER_B_ID);
    expect(oauthErrorOf(agentB)).toBeNull();

    // The precondition, and the whole reason this guard has teeth. Without the
    // second account row, `accounts[0]` and "the session's account" coincide.
    const users = await pool.query('select id from "user"');
    expect(users.rowCount, "implicit account linking did not happen").toBe(1);
    const accounts = await pool.query<{ providerId: string; accountId: string }>(
      'select "providerId", "accountId" from "account" order by "createdAt"',
    );
    expect(accounts.rows.map((row) => row.providerId)).toEqual([
      PRE_4B_PROVIDER_ID,
      PROVIDER_B_ID,
    ]);
    expect(accounts.rows.map((row) => row.accountId)).toEqual([SUB_A, SUB_B]);
    // Account A is FIRST. `accounts[0]` is therefore A, which is what makes the
    // mutation observable rather than accidentally correct.
    expect(accounts.rows[0]?.accountId).toBe(SUB_A);

    const sessions = await pool.query<{ providerId: string | null }>(
      `select "${SESSION_PROVIDER_FIELD}" from "session" order by "createdAt"`,
    );
    expect(sessions.rows.map((row) => row.providerId)).toEqual([
      PRE_4B_PROVIDER_ID,
      PROVIDER_B_ID,
    ]);
  });

  test("minting from B's session gives B's `iss` AND B's `sub`", async () => {
    const agentA = await signIn(server.origin, PRE_4B_PROVIDER_ID);
    const agentB = await signIn(server.origin, PROVIDER_B_ID);
    const jwks = createRemoteJWKSet(new URL(server.jwksUrl));

    const tokenB = await mint(agentB, server.origin);
    expect(tokenB.status).toBe(200);
    const verifiedB = await jwtVerify(String(tokenB.token), jwks, {
      issuer: PROVIDER_B_ISSUER,
      audience: AUDIENCE,
      algorithms: [MOIRA_JWT_ALGORITHM],
    });
    expect(verifiedB.payload.iss).toBe(PROVIDER_B_ISSUER);
    expect(verifiedB.payload.sub).toBe(SUB_B);
    // THE ASSERTION WITH TEETH. Under "return the first account", this is A's
    // subject under B's issuer: a token that verifies, names a real human, and
    // resolves the WRONG `admin_identities` grant.
    expect(
      verifiedB.payload.sub,
      "the subject came from the other linked account. `iss` names B and `sub` names A, so " +
        "the token resolves an admin grant that belongs to a different (issuer, subject) pair.",
    ).not.toBe(SUB_A);

    // And A's session, concurrently, still mints A's pair — so this is not a
    // "last one wins" arrangement either.
    const tokenA = await mint(agentA, server.origin);
    expect(tokenA.status).toBe(200);
    const verifiedA = await jwtVerify(String(tokenA.token), jwks, {
      issuer: PRE_4B_ISSUER,
      audience: AUDIENCE,
      algorithms: [MOIRA_JWT_ALGORITHM],
    });
    expect(verifiedA.payload.iss).toBe(PRE_4B_ISSUER);
    expect(verifiedA.payload.sub).toBe(SUB_A);
    expect(verifiedA.payload.sub).not.toBe(verifiedB.payload.sub);
    expect(verifiedA.payload.iss).not.toBe(verifiedB.payload.iss);
  });

  test("ONE HUMAN, TWO GRANTS — the consequence wave 4 ships and does not mitigate", async () => {
    // Recorded as an executable statement rather than only in the ledger. The
    // same person now holds two distinct `(issuer, subject)` pairs, so they hold
    // two `admin_identities` rows with no column linking them: revocation is per
    // grant, and `admin_identities_single_active_primary` is unique on
    // `(is_primary)` GLOBALLY, so they are primary through at most one of them.
    const agentA = await signIn(server.origin, PRE_4B_PROVIDER_ID);
    const agentB = await signIn(server.origin, PROVIDER_B_ID);
    const jwks = createRemoteJWKSet(new URL(server.jwksUrl));

    const pairs = await Promise.all(
      [agentA, agentB].map(async (agent) => {
        const minted = await mint(agent, server.origin);
        const [, payload] = String(minted.token).split(".");
        const claims = JSON.parse(Buffer.from(String(payload), "base64url").toString("utf8")) as {
          iss: string;
          sub: string;
          email: string;
        };
        // Verified against the JWKS too, so this is not reading an unsigned blob.
        await jwtVerify(String(minted.token), jwks, {
          issuer: claims.iss,
          audience: AUDIENCE,
          algorithms: [MOIRA_JWT_ALGORITHM],
        });
        return claims;
      }),
    );

    expect(new Set(pairs.map((pair) => pair.email)).size, "not the same human").toBe(1);
    expect(new Set(pairs.map((pair) => `${pair.iss}\n${pair.sub}`)).size).toBe(2);
  });
});

describe("the guard file itself", () => {
  test("names the database suite so a skipped run is visible", () => {
    // `describeDatabase` is `describe.skipIf(DATABASE_TESTS_SKIPPED)`, and
    // `console-db-availability.test.ts` FAILS when the skip is set. This
    // assertion exists so that a reader of this file's output can tell whether
    // the three guards above ran at all.
    expect(DATABASE_TESTS_SKIPPED, "CONSOLE_SKIP_DB_TESTS is set: G8/G9/G10 did NOT run").toBe(
      false,
    );
  });
});
