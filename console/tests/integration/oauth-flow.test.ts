// The whole sign-in path, end to end, against a real OIDC provider.
//
// ============================================================================
// WHAT IS REAL HERE AND WHAT IS MOCKED
// ============================================================================
//
// REAL: two TLS servers on real sockets; a real authorization-code redirect
// chain across an origin boundary; a real PKCE challenge/verifier pair; a real
// client-secret check at the token endpoint; real ES256 signing on both sides;
// a real JWKS fetch by `jose`'s `createRemoteJWKSet`; the shipped
// `createConsoleAuth` factory with the shipped plugin set.
//
// MOCKED: the identity provider's *identity*. It is not Google. No Google client
// id or secret exists for this project and none is requested, so the console is
// built against a provider that behaves like one. What that cannot cover is
// listed in `tests/support/mock-idp.ts` and deferred explicitly — not faked
// green.
//
// The verification the last test performs is Moira's verification, reproduced:
// Moira fetches the console's registered `jwks_url`, resolves the signing key by
// `kid`, checks `alg` against the issuer's `allowed_algorithms`, checks `iss`
// against the registered issuer string, checks `aud` against
// `expected_audiences`, and reads `sub` to look up the `admin_identities` grant.
// Every one of those is asserted below against a token minted by the real
// plugin and fetched over a real socket.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { createRemoteJWKSet, decodeProtectedHeader, jwtVerify } from "jose";
import { memoryAdapter } from "better-auth/adapters/memory";

import { CONSOLE_OAUTH_PROVIDER_ID, type ResolvedAuthConfig } from "@/lib/auth-config";
import {
  createConsoleMemoryDatabase,
  MissingIdpSubjectError,
  MOIRA_JWT_ALGORITHM,
  readIdpSubject,
} from "@/lib/auth";
import { readConsoleEnv, type ConsoleEnv } from "@/lib/env";
import {
  checkSession,
  mintMoiraToken,
  SESSION_REJECTION_MESSAGE_KEYS,
} from "@/lib/moira-session";

import { createBrowserAgent } from "../support/browser-agent";
import {
  reserveConsolePort,
  startConsoleServer,
  type ConsoleServer,
} from "../support/console-server";
import { trustFixtureCa, untrustFixtureCa } from "../support/fixture-tls";
import { restoreDomWhatwgGlobals, useNativeWhatwgGlobals } from "../support/native-globals";
import { startMockIdp, type MockIdp } from "../support/mock-idp";

/* -------------------------------------------------------------------------- */
/* Fixture constants                                                          */
/* -------------------------------------------------------------------------- */

const CLIENT_ID = "moira-console.apps.mock-idp.test";
const CLIENT_SECRET = "mock-idp-client-secret-do-not-reuse";
const OPERATOR = {
  sub: "mock-idp-subject-0f3a91",
  email: "operator@example.com",
  emailVerified: true,
  name: "Console Operator",
};
const AUDIENCE = "moira-admin-api";

function envFor(consoleOrigin: string): ConsoleEnv {
  return readConsoleEnv({
    NODE_ENV: "test",
    MOIRA_API_URL: "https://moira.invalid",
    CONSOLE_PUBLIC_ORIGIN: consoleOrigin,
    MOIRA_ADMIN_API_AUDIENCE: AUDIENCE,
    BETTER_AUTH_SECRET: "fixture-better-auth-secret-at-least-32-chars",
    // `openssl rand -base64 32`, for a fixture. Never a real deployment value.
    CONSOLE_SECRET_ENCRYPTION_KEY: Buffer.alloc(32, 7).toString("base64"),
  });
}

/**
 * The INCUMBENT provider, as wave 4B resolves it.
 *
 * `consoleIssuer` is `env.bffIssuerUrl` — not a special case in the code, but
 * the definition: the incumbent is the provider bound to the `bffIssuerUrl`
 * trusted issuer, so it keeps that string and `moira-console-idp` automatically.
 * Every assertion below that expects `iss === env.bffIssuerUrl` is asserting
 * exactly that, and would still pass under a `definePayload` that omitted `iss`
 * entirely — which is why the multi-provider file (`multi-provider.test.ts`)
 * owns guard G8 and this one does not claim to.
 */
function configFor(idp: MockIdp, clientSecret: string, env: ConsoleEnv): ResolvedAuthConfig {
  return {
    providerId: CONSOLE_OAUTH_PROVIDER_ID,
    consoleIssuer: env.bffIssuerUrl,
    method: "generic_oidc",
    moiraProviderId: "11111111-1111-4111-8111-111111111111",
    moiraProviderVersion: 3,
    // The IDP's issuer. Never the console's — the console's own issuer is
    // `env.bffIssuerUrl`, and conflating the two is the B1 defect.
    issuer: idp.issuer,
    discoveryUrl: idp.discoveryUrl,
    authorizationUrl: idp.authorizationUrl,
    tokenUrl: idp.tokenUrl,
    userInfoUrl: idp.userInfoUrl,
    clientId: CLIENT_ID,
    clientSecret,
    scopes: ["openid", "email", "profile"],
    allowedEmailDomains: ["example.com"],
    trustedJwtIssuerId: "22222222-2222-4222-8222-222222222222",
  };
}

/* -------------------------------------------------------------------------- */
/* The flow, as a reusable driver                                             */
/* -------------------------------------------------------------------------- */

interface SignInOutcome {
  readonly finalStatus: number;
  readonly finalUrl: string;
  readonly agent: ReturnType<typeof createBrowserAgent>;
  readonly errorParam: string | null;
}

async function signIn(consoleOrigin: string): Promise<SignInOutcome> {
  const agent = createBrowserAgent();

  // Step 1 — ask the console for an authorization URL. This is the endpoint the
  // sign-in button posts to.
  const start = await agent.request(`${consoleOrigin}/api/auth/sign-in/oauth2`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ providerId: CONSOLE_OAUTH_PROVIDER_ID, callbackURL: "/" }),
  });
  expect(start.status).toBe(200);
  const { url } = (await start.json()) as { url: string };
  expect(url).toStartWith("https://localhost:");

  // Step 2 — follow it. Two origins, at least two redirects: IdP `/authorize`
  // 302s to the console's callback, which 302s to `callbackURL`.
  const final = await agent.navigate(url);
  const last = agent.hops.at(-1);

  const errorParam = (() => {
    for (const hop of agent.hops) {
      if (hop.location === null) continue;
      const target = new URL(hop.location, hop.url);
      const error = target.searchParams.get("error");
      if (error !== null) return error;
    }
    return null;
  })();

  return {
    finalStatus: final.status,
    finalUrl: last?.url ?? "",
    agent,
    errorParam,
  };
}

/* -------------------------------------------------------------------------- */
/* Happy path                                                                 */
/* -------------------------------------------------------------------------- */

describe("OAuth sign-in against a real mock IdP", () => {
  let idp: MockIdp;
  let consoleServer: ConsoleServer;
  let env: ConsoleEnv;

  beforeAll(async () => {
    // Server-side suite: happy-dom's Headers silently discards `Set-Cookie`.
    // See `native-globals.ts`.
    useNativeWhatwgGlobals();

    const port = reserveConsolePort();
    const consoleOrigin = `https://localhost:${port}`;
    trustFixtureCa(consoleOrigin);

    idp = await startMockIdp({
      clientId: CLIENT_ID,
      clientSecret: CLIENT_SECRET,
      user: OPERATOR,
    });
    trustFixtureCa(idp.origin);

    env = envFor(consoleOrigin);
    consoleServer = startConsoleServer(
      {
        env,
        configs: [configFor(idp, CLIENT_SECRET, env)],
        database: memoryAdapter(createConsoleMemoryDatabase()),
      },
      port,
    );
  });

  afterAll(() => {
    consoleServer?.stop();
    idp?.stop();
    untrustFixtureCa();
    restoreDomWhatwgGlobals();
  });

  test("the provider's discovery document is fetched over TLS, not assumed", async () => {
    const response = await fetch(idp.discoveryUrl);
    expect(response.status).toBe(200);
    const discovery = (await response.json()) as Record<string, unknown>;
    expect(discovery["issuer"]).toBe(idp.issuer);
    expect(discovery["code_challenge_methods_supported"]).toEqual(["S256"]);
    // https, because Moira's `validate_https_url` has no allow_http escape hatch
    // on the provider path.
    expect(String(discovery["token_endpoint"])).toStartWith("https://");
  });

  test("a full authorization-code exchange establishes a console session", async () => {
    const outcome = await signIn(consoleServer.origin);
    expect(outcome.errorParam).toBeNull();
    expect(outcome.finalStatus).toBe(200);

    // The provider actually served the flow — this is the evidence that nothing
    // short-circuited it.
    expect(idp.routes()).toContain("GET /.well-known/openid-configuration");
    expect(idp.routes()).toContain("GET /authorize");
    expect(idp.routes()).toContain("POST /token");

    // And NOT `/userinfo` — an assertion that documents real behaviour rather
    // than a wish. Better Auth's generic-oauth provider reads the ID token's
    // claims when it carries both `sub` and `email`, and only calls the userinfo
    // endpoint otherwise. The next test covers that other branch.
    expect(idp.routes()).not.toContain("GET /userinfo");

    // And a session cookie came back.
    const cookies = outcome.agent.cookieNames().join(",");
    expect(cookies).toContain("session_token");
  });

  test("PKCE is exercised: the token endpoint saw a code_verifier", async () => {
    // The mock's `/token` refuses `invalid_grant` when a `code_challenge` was
    // presented at `/authorize` and the verifier does not hash to it, so a
    // successful exchange in the previous test is itself the proof. This test
    // asserts the challenge was sent at all, which is the part a silent
    // `pkce: false` would remove.
    const outcome = await signIn(consoleServer.origin);
    expect(outcome.errorParam).toBeNull();
    const authorizeHop = outcome.agent.hops.find((hop) => hop.url.includes("/authorize"));
    expect(authorizeHop).toBeDefined();
    const challenge = new URL(authorizeHop?.url ?? "").searchParams;
    expect(challenge.get("code_challenge_method")).toBe("S256");
    expect(challenge.get("code_challenge")).toBeString();
    expect(challenge.get("state")).toBeString();
  });

  /* ------------------------------------------------------------------------ */
  /* The Moira-bound token — this is the verification that matters             */
  /* ------------------------------------------------------------------------ */

  test("the minted JWT verifies exactly the way Moira verifies it", async () => {
    const outcome = await signIn(consoleServer.origin);
    expect(outcome.errorParam).toBeNull();

    const tokenResponse = await outcome.agent.request(`${consoleServer.origin}/api/auth/token`);
    expect(tokenResponse.status).toBe(200);
    const { token } = (await tokenResponse.json()) as { token: string };
    expect(token).toBeString();

    // 1. `alg` must be the pinned one. Moira's issuer row carries
    //    `allowed_algorithms: ["ES256"]`, and a token signed with anything else
    //    is refused before the signature is even checked.
    expect(decodeProtectedHeader(token).alg).toBe(MOIRA_JWT_ALGORITHM);
    expect(decodeProtectedHeader(token).kid).toBeString();

    // 2. Fetch the JWKS the way Moira does — over the network, by URL, resolving
    //    the key by `kid`. Not by reading the plugin's internals.
    const jwks = createRemoteJWKSet(new URL(consoleServer.jwksUrl));
    const { payload } = await jwtVerify(token, jwks, {
      issuer: env.bffIssuerUrl,
      audience: env.adminApiAudience,
      algorithms: [MOIRA_JWT_ALGORITHM],
    });

    // 3. `iss` is the CONSOLE's issuer, not the IdP's. This is the distinction
    //    B1 turns on: the claim body names the console's issuer, so the
    //    `auth_provider_settings` row must be bound by `trusted_jwt_issuer_id`
    //    rather than by a matching `issuer` column.
    expect(payload.iss).toBe(env.bffIssuerUrl);
    expect(payload.iss).not.toBe(idp.issuer);

    // 4. `sub` is the IDP's stable subject — the same value the claim body sends
    //    as `subject`, and therefore the grant key. If this ever becomes the
    //    Better Auth user id, every existing grant is orphaned and no second
    //    claim is possible (409 admin_identity_already_claimed).
    expect(payload.sub).toBe(OPERATOR.sub);

    // 5. `aud` non-empty and correct. An empty `expected_audiences` makes Moira
    //    skip audience validation entirely, so "no audience" is worse than a
    //    wrong one.
    expect(payload.aud).toBe(AUDIENCE);

    expect(payload["email"]).toBe(OPERATOR.email);
    expect(payload["email_verified"]).toBe(true);
  });

  test("the JWKS document is a real published document with a usable key", async () => {
    const response = await fetch(consoleServer.jwksUrl);
    expect(response.status).toBe(200);
    const document = (await response.json()) as { keys: Record<string, unknown>[] };
    expect(document.keys.length).toBeGreaterThan(0);
    const key = document.keys[0];
    expect(key?.["kty"]).toBe("EC");
    expect(key?.["crv"]).toBe("P-256");
    expect(key?.["alg"]).toBe(MOIRA_JWT_ALGORITHM);
    // No private material in a public document. The plugin encrypts the private
    // half at rest; this asserts it never reaches the published one.
    expect(key?.["d"]).toBeUndefined();
  });

  test("the session passes the console's own allow-list check", async () => {
    const check = checkSession(
      { email: OPERATOR.email, emailVerified: true, idpSubject: OPERATOR.sub },
      { allowedEmailDomains: ["example.com"] },
    );
    expect(check.ok).toBe(true);

    const outsider = checkSession(
      { email: "someone@evilexample.com", emailVerified: true, idpSubject: "x" },
      { allowedEmailDomains: ["example.com"] },
    );
    expect(outsider.ok).toBe(false);
  });
});

/* -------------------------------------------------------------------------- */
/* D7: the console owns the secret, so a wrong one must actually fail          */
/* -------------------------------------------------------------------------- */

describe("the console-held client secret is load-bearing", () => {
  let idp: MockIdp;
  let consoleServer: ConsoleServer;

  beforeAll(async () => {
    // Server-side suite: happy-dom's Headers silently discards `Set-Cookie`.
    // See `native-globals.ts`.
    useNativeWhatwgGlobals();

    const port = reserveConsolePort();
    const consoleOrigin = `https://localhost:${port}`;
    trustFixtureCa(consoleOrigin);

    idp = await startMockIdp({
      clientId: CLIENT_ID,
      clientSecret: CLIENT_SECRET,
      user: OPERATOR,
    });
    trustFixtureCa(idp.origin);

    consoleServer = startConsoleServer(
      {
        env: envFor(consoleOrigin),
        // The drift D7 makes possible: Moira's row is right, the console's
        // stored secret is stale. Nothing on the Moira side can detect this,
        // because Moira never held the secret.
        configs: [configFor(idp, "the-wrong-secret", envFor(consoleOrigin))],
        database: memoryAdapter(createConsoleMemoryDatabase()),
      },
      port,
    );
  });

  afterAll(() => {
    consoleServer?.stop();
    idp?.stop();
    untrustFixtureCa();
    restoreDomWhatwgGlobals();
  });

  test("a stale client secret fails the code exchange rather than signing anyone in", async () => {
    const outcome = await signIn(consoleServer.origin);

    // The provider was reached and refused: the exchange was attempted for real.
    expect(idp.routes()).toContain("POST /token");
    // No userinfo call, because the token exchange never produced a token.
    expect(idp.routes()).not.toContain("GET /userinfo");

    expect(outcome.errorParam).toBe("oauth_code_verification_failed");
    expect(outcome.agent.cookieNames().join(",")).not.toContain("session_token");
  });
});

/* -------------------------------------------------------------------------- */
/* The userinfo branch — the one the happy path does NOT take                  */
/* -------------------------------------------------------------------------- */

describe("identity falls back to the userinfo endpoint when the ID token is thin", () => {
  let idp: MockIdp;
  let consoleServer: ConsoleServer;
  let env: ConsoleEnv;

  beforeAll(async () => {
    useNativeWhatwgGlobals();

    const port = reserveConsolePort();
    const consoleOrigin = `https://localhost:${port}`;
    trustFixtureCa(consoleOrigin);

    idp = await startMockIdp({
      clientId: CLIENT_ID,
      clientSecret: CLIENT_SECRET,
      user: OPERATOR,
      // Better Auth uses the ID token only when it carries `sub` AND `email`.
      idTokenOmitsEmail: true,
    });
    trustFixtureCa(idp.origin);

    env = envFor(consoleOrigin);
    consoleServer = startConsoleServer(
      {
        env,
        configs: [configFor(idp, CLIENT_SECRET, env)],
        database: memoryAdapter(createConsoleMemoryDatabase()),
      },
      port,
    );
  });

  afterAll(() => {
    consoleServer?.stop();
    idp?.stop();
    untrustFixtureCa();
    restoreDomWhatwgGlobals();
  });

  test("the userinfo endpoint is called, and `sub` still lands in the minted token", async () => {
    const outcome = await signIn(consoleServer.origin);
    expect(outcome.errorParam).toBeNull();
    expect(idp.routes()).toContain("GET /userinfo");

    const tokenResponse = await outcome.agent.request(`${consoleServer.origin}/api/auth/token`);
    expect(tokenResponse.status).toBe(200);
    const { token } = (await tokenResponse.json()) as { token: string };

    const jwks = createRemoteJWKSet(new URL(consoleServer.jwksUrl));
    const { payload } = await jwtVerify(token, jwks, {
      issuer: env.bffIssuerUrl,
      audience: env.adminApiAudience,
      algorithms: [MOIRA_JWT_ALGORITHM],
    });
    // Same subject from the other branch. `sub` is read from
    // `account.accountId`, which Better Auth populates identically either way —
    // that invariance is the point of reading the account row rather than a
    // profile-mapped user field.
    expect(payload.sub).toBe(OPERATOR.sub);
  });
});

/* -------------------------------------------------------------------------- */
/* No IdP subject => refuse the sign-in, rather than invent one                */
/* -------------------------------------------------------------------------- */

describe("a provider that supplies no subject cannot produce a console session", () => {
  let idp: MockIdp;
  let consoleServer: ConsoleServer;

  beforeAll(async () => {
    useNativeWhatwgGlobals();

    const port = reserveConsolePort();
    const consoleOrigin = `https://localhost:${port}`;
    trustFixtureCa(consoleOrigin);

    idp = await startMockIdp({
      clientId: CLIENT_ID,
      clientSecret: CLIENT_SECRET,
      user: OPERATOR,
      omitSubject: true,
    });
    trustFixtureCa(idp.origin);

    consoleServer = startConsoleServer(
      {
        env: envFor(consoleOrigin),
        configs: [configFor(idp, CLIENT_SECRET, envFor(consoleOrigin))],
        database: memoryAdapter(createConsoleMemoryDatabase()),
      },
      port,
    );
  });

  afterAll(() => {
    consoleServer?.stop();
    idp?.stop();
    untrustFixtureCa();
    restoreDomWhatwgGlobals();
  });

  test("sign-in is refused rather than keyed to a console-invented id", async () => {
    const outcome = await signIn(consoleServer.origin);
    // What must NOT happen is a session whose `sub` becomes the console's own
    // user id: that token verifies against the console's JWKS and then matches
    // no `admin_identities` grant, producing a bare 403 from Moira with nothing
    // locally to explain it.
    expect(outcome.errorParam).toBe("id_is_missing");
    expect(outcome.agent.cookieNames().join(",")).not.toContain("session_token");
  });
});

/* -------------------------------------------------------------------------- */
/* And the same refusal at mint time, without an IdP                          */
/* -------------------------------------------------------------------------- */

describe("readIdpSubject", () => {
  const context = (accounts: { accountId: string; providerId: string }[]) => ({
    internalAdapter: { findAccountByUserId: async () => accounts },
  });

  test("returns the linked account's subject", async () => {
    const subject = await readIdpSubject(
      context([{ accountId: "idp-sub-1", providerId: CONSOLE_OAUTH_PROVIDER_ID }]),
      CONSOLE_OAUTH_PROVIDER_ID,
      "console-user-1",
    );
    expect(subject).toBe("idp-sub-1");
  });

  test("ignores accounts from a different provider rather than guessing", async () => {
    await expect(
      readIdpSubject(
        context([{ accountId: "other-sub", providerId: "some-other-provider" }]),
        CONSOLE_OAUTH_PROVIDER_ID,
        "console-user-1",
      ),
    ).rejects.toBeInstanceOf(MissingIdpSubjectError);
  });

  test("throws rather than falling back to the console's own user id", async () => {
    await expect(
      readIdpSubject(context([]), CONSOLE_OAUTH_PROVIDER_ID, "console-user-1"),
    ).rejects.toBeInstanceOf(MissingIdpSubjectError);
  });
});

/* -------------------------------------------------------------------------- */
/* G12 — the CREDENTIAL gate, not the page gate (finding F25)                  */
/* -------------------------------------------------------------------------- */

/**
 * The console's `allowed_email_domains` check had eleven green unit assertions
 * and no shipped caller. This is the wire-level test that would have failed then
 * and passes now, and it is deliberately built so that a PAGE-level wiring would
 * NOT satisfy it.
 *
 * The human here completes the whole OAuth flow successfully — the IdP knows
 * them, the code exchange works, the session cookie is set — and is simply
 * outside the deployment's allow-list. Under a layout-only gate they would be
 * redirected in a browser while `GET /api/auth/token` handed the same cookie a
 * working admin credential. That is the arrangement this asserts against.
 */
describe("a session outside the allow-list cannot be exchanged for a Moira credential", () => {
  let idp: MockIdp;
  let consoleServer: ConsoleServer;

  beforeAll(async () => {
    useNativeWhatwgGlobals();

    const port = reserveConsolePort();
    const consoleOrigin = `https://localhost:${port}`;
    trustFixtureCa(consoleOrigin);

    idp = await startMockIdp({ clientId: CLIENT_ID, clientSecret: CLIENT_SECRET, user: OPERATOR });
    trustFixtureCa(idp.origin);

    consoleServer = startConsoleServer(
      {
        env: envFor(consoleOrigin),
        configs: [
          {
            ...configFor(idp, CLIENT_SECRET, envFor(consoleOrigin)),
            // OPERATOR is `operator@example.com`. The deployment admits `corp.test`
            // and nothing else, so sign-in succeeds and admission does not.
            allowedEmailDomains: ["corp.test"],
          },
        ],
        database: memoryAdapter(createConsoleMemoryDatabase()),
      },
      port,
    );
  });

  afterAll(() => {
    consoleServer?.stop();
    idp?.stop();
    untrustFixtureCa();
    restoreDomWhatwgGlobals();
  });

  test("the OAuth flow itself still succeeds — the refusal is at the credential, not the door", async () => {
    const outcome = await signIn(consoleServer.origin);
    expect(outcome.errorParam).toBeNull();
    expect(outcome.finalStatus).toBe(200);
    // The premise, asserted rather than assumed: without a real session cookie
    // the token request below would be refused for the ordinary reason and the
    // test would pass while proving nothing.
    expect(outcome.agent.cookieNames().join(",")).toContain("session_token");
  });

  test("GET /api/auth/token answers 403 with a keyed body and NO token", async () => {
    const outcome = await signIn(consoleServer.origin);
    expect(outcome.agent.cookieNames().join(",")).toContain("session_token");

    const response = await outcome.agent.request(`${consoleServer.origin}/api/auth/token`);

    expect(response.status).toBe(403);
    const body = (await response.json()) as Record<string, unknown>;
    // The assertion that matters: no credential came back. A 403 whose body also
    // carried a usable token would satisfy a status-only test.
    expect(body["token"]).toBeUndefined();
    expect(JSON.stringify(body)).not.toContain("eyJ");
    const error = body["error"] as Record<string, unknown> | undefined;
    expect(error?.["code"]).toBe("email_domain_not_allowed");
    expect(error?.["message_key"]).toBe(
      SESSION_REJECTION_MESSAGE_KEYS.email_domain_not_allowed,
    );
  });

  test("mintMoiraToken throws for the same session — the backstop under the route", async () => {
    const outcome = await signIn(consoleServer.origin);
    const headers = new Headers({
      cookie: outcome.agent.cookieHeaderFor(consoleServer.origin),
    });

    // Straight at `auth.api.getToken`, bypassing the route handler entirely.
    // This is every server-side minting path — a page, a server action, a future
    // API route — and none of them passes through `app/api/auth/[...all]`. If
    // only the route were wired, this call would return a token.
    await expect(mintMoiraToken(consoleServer.auth, headers)).rejects.toThrow();
  });

  test("the SAME session is admitted once its domain is allowed", async () => {
    // The negative control. Without it, a check that refused everybody — or a
    // token endpoint broken for an unrelated reason — would pass every assertion
    // above.
    const port = reserveConsolePort();
    const origin = `https://localhost:${port}`;
    trustFixtureCa(origin);
    const permissive = startConsoleServer(
      {
        env: envFor(origin),
        configs: [
          { ...configFor(idp, CLIENT_SECRET, envFor(origin)), allowedEmailDomains: ["example.com"] },
        ],
        database: memoryAdapter(createConsoleMemoryDatabase()),
      },
      port,
    );
    try {
      const outcome = await signIn(permissive.origin);
      const response = await outcome.agent.request(`${permissive.origin}/api/auth/token`);
      expect(response.status).toBe(200);
      const { token } = (await response.json()) as { token?: string };
      expect(token).toBeString();
    } finally {
      permissive.stop();
    }
  });
});
