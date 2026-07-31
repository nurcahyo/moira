// ============================================================================
// SPIKE — plan 09 wave 4, task T0. THIS IS NOT SHIPPED COVERAGE.
// ============================================================================
//
// THE QUESTION. Can the authenticating account's `providerId` be made available
// to the token minter for the current session, in better-auth 1.6.25 as
// vendored here — either by stamping it onto the session at creation, or by
// reading it inside `definePayload` / `getSubject`?
//
// WHY IT MATTERS. Finding F24: `admin_identities` is keyed on
// `(issuer, subject)` where `issuer` is the console's and identical for every
// provider, so two IdPs returning the same `sub` collapse into one admin grant.
// Wave 4's Option A' closes F24 by minting a DIFFERENT `iss` per provider. That
// only works if the minter knows which provider authenticated the session. If
// it cannot, the console mints a token whose `iss` names the wrong provider —
// reproducing F24 silently, while every existing test stays green.
//
// WHY IT IS NOT OBVIOUS. Better Auth links accounts implicitly by verified
// email (`accountLinking.enabled` defaults on). So one `user` row can carry two
// `account` rows with different `providerId`s; the `session` model has no
// provider column; `account` is 1:N on `userId`; and
// `dist/oauth2/link-account.mjs::handleOAuthUserInfo` ends with a bare
// `createSession(user.id)` that passes no provider and leaves `createSession`'s
// own `override` parameter undefined.
//
// WHAT COUNTS AS A PASS, and what this file therefore refuses to settle for.
// A single-account fixture passes whether or not the mechanism works: with one
// account there is nothing to confuse. The decision's guard table names that as
// the toothless version to reject. So the load-bearing test here is
// `G10` below — ONE user with TWO linked accounts, signed in through provider
// B, asserting the minted `iss` AND `sub` both name B.
//
// TWO ANSWERS THAT WERE FORBIDDEN, and are not used anywhere in this spike:
//   * the most-recently-updated-account heuristic — a guess, and wrong exactly
//     when two accounts are linked, which is the case this exists to handle;
//   * disabling implicit account linking to force 1:1 — a second provider then
//     returns "account not linked" for precisely the humans multi-provider
//     exists to serve.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { createRemoteJWKSet, decodeJwt, decodeProtectedHeader, jwtVerify } from "jose";
import { memoryAdapter } from "better-auth/adapters/memory";

import { createConsoleMemoryDatabase, MOIRA_JWT_ALGORITHM } from "@/lib/auth";
import { AUTH_BASE_PATH, AUTH_JWKS_PATH, readConsoleEnv, type ConsoleEnv } from "@/lib/env";

import { createBrowserAgent } from "../support/browser-agent";
import { reserveConsolePort } from "../support/console-server";
import { fixtureTls, trustFixtureCa, untrustFixtureCa } from "../support/fixture-tls";
import { restoreDomWhatwgGlobals, useNativeWhatwgGlobals } from "../support/native-globals";
import { startMockIdp, type MockIdp } from "../support/mock-idp";

import {
  createSpikeAuth,
  SESSION_PROVIDER_FIELD,
  type SessionCreateObservation,
  type SpikeAuth,
  type SpikeProvider,
} from "./w4-t0-spike-auth";

/* -------------------------------------------------------------------------- */
/* Fixture constants                                                          */
/* -------------------------------------------------------------------------- */

const AUDIENCE = "moira-admin-api";

/** The incumbent. Its Better Auth id must never change — see plan 09 T7. */
const PROVIDER_A_ID = "moira-console-idp";
/** A second provider, as 4B would name it: `moira-console-idp-<slug>`. */
const PROVIDER_B_ID = "moira-console-idp-contractors";

/**
 * The two console issuer strings, as T7 derives them.
 *
 * These stand in for the `issuer` column of two `trusted_jwt_issuers` rows
 * sharing one `jwks_url`. They are what makes `(issuer, subject)` distinct per
 * provider in `admin_identities`, and therefore what closes F24.
 */
const CONSOLE_ISSUER_A = "https://console.test/idp/corp";
const CONSOLE_ISSUER_B = "https://console.test/idp/contractors";

const CLIENT_ID_A = "moira-console-a.apps.mock-idp.test";
const CLIENT_SECRET_A = "mock-idp-a-client-secret-do-not-reuse";
const CLIENT_ID_B = "moira-console-b.apps.mock-idp.test";
const CLIENT_SECRET_B = "mock-idp-b-client-secret-do-not-reuse";

function envFor(consoleOrigin: string): ConsoleEnv {
  return readConsoleEnv({
    NODE_ENV: "test",
    MOIRA_API_URL: "https://moira.invalid",
    CONSOLE_PUBLIC_ORIGIN: consoleOrigin,
    MOIRA_ADMIN_API_AUDIENCE: AUDIENCE,
    BETTER_AUTH_SECRET: "fixture-better-auth-secret-at-least-32-chars",
    CONSOLE_SECRET_ENCRYPTION_KEY: Buffer.alloc(32, 7).toString("base64"),
  });
}

/* -------------------------------------------------------------------------- */
/* A console serving the spike instance, on a real socket                     */
/* -------------------------------------------------------------------------- */

interface SpikeServer {
  readonly origin: string;
  readonly jwksUrl: string;
  stop(): void;
}

function startSpikeServer(auth: SpikeAuth, port: number): SpikeServer {
  const tls = fixtureTls();
  const server = Bun.serve({
    port,
    hostname: "127.0.0.1",
    tls: { key: tls.key, cert: tls.cert },
    async fetch(request): Promise<Response> {
      const url = new URL(request.url);
      if (url.pathname.startsWith(AUTH_BASE_PATH)) return auth.handler(request);
      return new Response("console", { status: 200, headers: { "content-type": "text/plain" } });
    },
  });
  return {
    origin: `https://localhost:${server.port}`,
    jwksUrl: `https://localhost:${server.port}${AUTH_BASE_PATH}${AUTH_JWKS_PATH}`,
    stop: () => {
      server.stop(true);
    },
  };
}

/* -------------------------------------------------------------------------- */
/* Driving one full authorization-code flow                                   */
/* -------------------------------------------------------------------------- */

interface SignedInAgent {
  readonly agent: ReturnType<typeof createBrowserAgent>;
  readonly errorParam: string | null;
}

async function signIn(
  consoleOrigin: string,
  providerId: string,
  reuse?: ReturnType<typeof createBrowserAgent>,
): Promise<SignedInAgent> {
  const agent = reuse ?? createBrowserAgent();

  const start = await agent.request(`${consoleOrigin}/api/auth/sign-in/oauth2`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ providerId, callbackURL: "/" }),
  });
  expect(start.status).toBe(200);
  const { url } = (await start.json()) as { url: string };

  await agent.navigate(url);

  const errorParam = (() => {
    for (const hop of agent.hops) {
      if (hop.location === null) continue;
      const error = new URL(hop.location, hop.url).searchParams.get("error");
      if (error !== null) return error;
    }
    return null;
  })();

  return { agent, errorParam };
}

interface SessionPayload {
  readonly session: Record<string, unknown>;
  readonly user: { readonly id: string; readonly email: string };
}

async function readSession(
  consoleOrigin: string,
  agent: ReturnType<typeof createBrowserAgent>,
): Promise<SessionPayload> {
  const response = await agent.request(`${consoleOrigin}/api/auth/get-session`);
  expect(response.status).toBe(200);
  return (await response.json()) as SessionPayload;
}

async function mintToken(
  consoleOrigin: string,
  agent: ReturnType<typeof createBrowserAgent>,
): Promise<{ status: number; token: string | null }> {
  const response = await agent.request(`${consoleOrigin}/api/auth/token`);
  const body = (await response.json().catch(() => null)) as { token?: unknown } | null;
  const token = typeof body?.token === "string" ? body.token : null;
  return { status: response.status, token };
}

function providersFor(idpA: MockIdp, idpB: MockIdp): SpikeProvider[] {
  return [
    {
      providerId: PROVIDER_A_ID,
      consoleIssuer: CONSOLE_ISSUER_A,
      clientId: CLIENT_ID_A,
      clientSecret: CLIENT_SECRET_A,
      discoveryUrl: idpA.discoveryUrl,
      scopes: ["openid", "email", "profile"],
    },
    {
      providerId: PROVIDER_B_ID,
      consoleIssuer: CONSOLE_ISSUER_B,
      clientId: CLIENT_ID_B,
      clientSecret: CLIENT_SECRET_B,
      discoveryUrl: idpB.discoveryUrl,
      scopes: ["openid", "email", "profile"],
    },
  ];
}

/* ========================================================================== */
/* PART 1 — the raw observation, and G8: two providers, two humans            */
/* ========================================================================== */

describe("W4-T0 part 1 — two providers mint two different issuers", () => {
  const SUB_A = "corp-idp-subject-11aa";
  const SUB_B = "contractor-idp-subject-22bb";

  let idpA: MockIdp;
  let idpB: MockIdp;
  let server: SpikeServer;
  let observations: SessionCreateObservation[];

  beforeAll(async () => {
    useNativeWhatwgGlobals();

    const port = reserveConsolePort();
    const consoleOrigin = `https://localhost:${port}`;
    trustFixtureCa(consoleOrigin);

    idpA = await startMockIdp({
      clientId: CLIENT_ID_A,
      clientSecret: CLIENT_SECRET_A,
      user: {
        sub: SUB_A,
        email: "employee@corp.test",
        emailVerified: true,
        name: "Corp Employee",
      },
    });
    idpB = await startMockIdp({
      clientId: CLIENT_ID_B,
      clientSecret: CLIENT_SECRET_B,
      user: {
        sub: SUB_B,
        email: "hired@contractor.test",
        emailVerified: true,
        name: "Contractor",
      },
    });
    trustFixtureCa(idpA.origin);
    trustFixtureCa(idpB.origin);

    observations = [];
    const auth = createSpikeAuth({
      env: envFor(consoleOrigin),
      providers: providersFor(idpA, idpB),
      database: memoryAdapter(createConsoleMemoryDatabase()),
      observations,
    });
    server = startSpikeServer(auth, port);
  });

  afterAll(() => {
    server?.stop();
    idpA?.stop();
    idpB?.stop();
    untrustFixtureCa();
    restoreDomWhatwgGlobals();
  });

  /* ---------------------------------------------------------------------- */
  /* THE SINGLE OBSERVATION THAT DECIDES THE SPIKE                           */
  /* ---------------------------------------------------------------------- */

  test("the endpoint context at session-create time carries the callback's providerId", async () => {
    const before = observations.length;
    const { errorParam } = await signIn(server.origin, PROVIDER_B_ID);
    expect(errorParam).toBeNull();

    const seen = observations.slice(before);
    // Exactly one session was created by this flow.
    expect(seen).toHaveLength(1);
    const only = seen[0];

    // 1. A `GenericEndpointContext` was available at all. `createWithHooks`
    //    spells this `await getCurrentAuthContext().catch(() => null)`, so a
    //    session created outside `runWithEndpointContext` would hand the hook
    //    `null` and the mechanism would have no input.
    expect(only?.hadContext).toBe(true);

    // 2. The context is the OAuth callback endpoint's. Note this is the route
    //    TEMPLATE, not the concrete request path: `dispatchAuthEndpoint` sets
    //    `path: endpoint.path`. That is why the provider must be read from
    //    `params` and never parsed out of `path` — `path.split("/").pop()`
    //    yields the literal string ":providerId".
    expect(only?.path).toBe("/oauth2/callback/:providerId");
    expect(String(only?.path)).toContain(":providerId");

    // 3. And the route parameter is populated, with the provider that actually
    //    authenticated. THIS IS THE GATE. `handleOAuthUserInfo` calls
    //    `createSession(user.id)` with no provider argument; better-call's
    //    router put `params` on the input, `dispatchAuthEndpoint` ran the
    //    handler inside `runWithEndpointContext(internalContext, …)`, and
    //    `createWithHooks` read it back out of AsyncLocalStorage.
    expect(only?.params).toBeTruthy();
    expect(only?.params?.[SESSION_PROVIDER_FIELD]).toBe(PROVIDER_B_ID);
  });

  test("the stamp is persisted on the session row and readable afterwards", async () => {
    const { agent, errorParam } = await signIn(server.origin, PROVIDER_A_ID);
    expect(errorParam).toBeNull();

    const { session, user } = await readSession(server.origin, agent);
    expect(user.email).toBe("employee@corp.test");
    // Read back through the session model — not out of the hook's closure.
    // `parseSessionOutput` filters against the session schema, so a column that
    // was written but not declared in `additionalFields` would vanish here.
    expect(session[SESSION_PROVIDER_FIELD]).toBe(PROVIDER_A_ID);
  });

  /* ---------------------------------------------------------------------- */
  /* G8 — the token's `iss` names the provider that authenticated the session */
  /* ---------------------------------------------------------------------- */

  test("G8: two flows through two IdPs mint two different, correct issuers", async () => {
    const flowA = await signIn(server.origin, PROVIDER_A_ID);
    expect(flowA.errorParam).toBeNull();
    const tokenA = await mintToken(server.origin, flowA.agent);
    expect(tokenA.status).toBe(200);
    expect(tokenA.token).toBeString();

    const flowB = await signIn(server.origin, PROVIDER_B_ID);
    expect(flowB.errorParam).toBeNull();
    const tokenB = await mintToken(server.origin, flowB.agent);
    expect(tokenB.status).toBe(200);
    expect(tokenB.token).toBeString();

    // Both verify against ONE JWKS — 4B ships one ES256 key pair and N issuer
    // strings, so "the token verifies" is true in the broken arrangement too.
    // That is why the assertion below is on `iss`, not on verification.
    const jwks = createRemoteJWKSet(new URL(server.jwksUrl));

    const verifiedA = await jwtVerify(String(tokenA.token), jwks, {
      issuer: CONSOLE_ISSUER_A,
      audience: AUDIENCE,
      algorithms: [MOIRA_JWT_ALGORITHM],
    });
    const verifiedB = await jwtVerify(String(tokenB.token), jwks, {
      issuer: CONSOLE_ISSUER_B,
      audience: AUDIENCE,
      algorithms: [MOIRA_JWT_ALGORITHM],
    });

    // The two issuers DIFFER. This is F24 as a test: with one console issuer
    // both tokens carry the same string and two IdPs returning the same `sub`
    // collapse into one `admin_identities` grant.
    expect(verifiedA.payload.iss).toBe(CONSOLE_ISSUER_A);
    expect(verifiedB.payload.iss).toBe(CONSOLE_ISSUER_B);
    expect(verifiedA.payload.iss).not.toBe(verifiedB.payload.iss);

    // Neither is the plugin's `options.jwt.issuer` fallback. `sign.mjs` spells
    // it `.setIssuer(iss ?? defaultIss)`, so if `definePayload` ever stopped
    // supplying `iss` both tokens would silently carry this sentinel.
    expect(verifiedA.payload.iss).not.toBe("https://spike.invalid/never-mint-this");
    expect(verifiedB.payload.iss).not.toBe("https://spike.invalid/never-mint-this");

    // And each `sub` is its own IdP's subject, not the console's user id.
    expect(verifiedA.payload.sub).toBe(SUB_A);
    expect(verifiedB.payload.sub).toBe(SUB_B);

    // One key pair, one `kid`, two issuers — recorded because it is the honest
    // limit of Option A': there is NO cryptographic separation between
    // providers, only a claim the console chooses.
    expect(decodeProtectedHeader(String(tokenA.token)).kid).toBe(
      decodeProtectedHeader(String(tokenB.token)).kid,
    );
  });
});

/* ========================================================================== */
/* PART 2 — G10: ONE user, TWO linked accounts. The case that has teeth.      */
/* ========================================================================== */

describe("W4-T0 part 2 — one human, two linked accounts", () => {
  // Same verified email at both IdPs, different subjects. This is what makes
  // Better Auth link the second account onto the existing user, and it is the
  // arrangement in which every "pick an account" heuristic goes wrong.
  const SHARED_EMAIL = "dual@corp.test";
  const SUB_A = "corp-idp-subject-aaaa1111";
  const SUB_B = "contractor-idp-subject-bbbb2222";

  let idpA: MockIdp;
  let idpB: MockIdp;
  let server: SpikeServer;
  let auth: SpikeAuth;

  beforeAll(async () => {
    useNativeWhatwgGlobals();

    const port = reserveConsolePort();
    const consoleOrigin = `https://localhost:${port}`;
    trustFixtureCa(consoleOrigin);

    idpA = await startMockIdp({
      clientId: CLIENT_ID_A,
      clientSecret: CLIENT_SECRET_A,
      user: { sub: SUB_A, email: SHARED_EMAIL, emailVerified: true, name: "Dual Identity" },
    });
    idpB = await startMockIdp({
      clientId: CLIENT_ID_B,
      clientSecret: CLIENT_SECRET_B,
      user: { sub: SUB_B, email: SHARED_EMAIL, emailVerified: true, name: "Dual Identity" },
    });
    trustFixtureCa(idpA.origin);
    trustFixtureCa(idpB.origin);

    auth = createSpikeAuth({
      env: envFor(consoleOrigin),
      providers: providersFor(idpA, idpB),
      database: memoryAdapter(createConsoleMemoryDatabase()),
    });
    server = startSpikeServer(auth, port);
  });

  afterAll(() => {
    server?.stop();
    idpA?.stop();
    idpB?.stop();
    untrustFixtureCa();
    restoreDomWhatwgGlobals();
  });

  test("G10: after implicit linking, provider B's session mints B's iss AND B's sub", async () => {
    // ---- 1. Sign in through A. Creates the user and account A. -------------
    const flowA = await signIn(server.origin, PROVIDER_A_ID);
    expect(flowA.errorParam).toBeNull();
    const sessionA = await readSession(server.origin, flowA.agent);
    expect(sessionA.user.email).toBe(SHARED_EMAIL);

    // ---- 2. Sign in through B, as a different browser. ---------------------
    // Better Auth's `handleOAuthUserInfo` finds the user by verified email and
    // links account B onto it, because `accountLinking.enabled` defaults on,
    // `disableImplicitLinking` is unset and `requireLocalEmailVerified`
    // defaults true with both sides verified.
    const flowB = await signIn(server.origin, PROVIDER_B_ID);
    expect(flowB.errorParam).toBeNull();
    const sessionB = await readSession(server.origin, flowB.agent);

    // ---- 3. THE PRECONDITION. Without this the test is toothless. ----------
    // One user row, two account rows. If linking had not happened this would be
    // two users, and every heuristic would look correct.
    expect(sessionB.user.id).toBe(sessionA.user.id);
    const context = (await (auth as unknown as { $context: Promise<unknown> }).$context) as {
      internalAdapter: {
        findAccountByUserId(userId: string): Promise<{ accountId: string; providerId: string }[]>;
      };
    };
    const accounts = await context.internalAdapter.findAccountByUserId(sessionB.user.id);
    expect(accounts).toHaveLength(2);
    expect(accounts.map((a) => a.providerId).sort()).toEqual([PROVIDER_A_ID, PROVIDER_B_ID].sort());
    // The two subjects genuinely differ, so `sub` can be wrong in a detectable
    // way. (F24's real-world sting is the opposite case — GitHub subjects are
    // short numeric strings that CAN coincide with another IdP's — which is why
    // `iss` has to carry the separation.)
    expect(accounts.map((a) => a.accountId).sort()).toEqual([SUB_A, SUB_B].sort());

    // ---- 4. The sessions disagree about their provider, correctly. ---------
    expect(sessionA.session[SESSION_PROVIDER_FIELD]).toBe(PROVIDER_A_ID);
    expect(sessionB.session[SESSION_PROVIDER_FIELD]).toBe(PROVIDER_B_ID);
    expect(sessionA.session["id"]).not.toBe(sessionB.session["id"]);

    // ---- 5. THE ANSWER. Mint from B's session. -----------------------------
    const mintedB = await mintToken(server.origin, flowB.agent);
    expect(mintedB.status).toBe(200);
    const jwks = createRemoteJWKSet(new URL(server.jwksUrl));
    const verifiedB = await jwtVerify(String(mintedB.token), jwks, {
      issuer: CONSOLE_ISSUER_B,
      audience: AUDIENCE,
      algorithms: [MOIRA_JWT_ALGORITHM],
    });
    expect(verifiedB.payload.iss).toBe(CONSOLE_ISSUER_B);
    expect(verifiedB.payload.sub).toBe(SUB_B);
    // Both halves name B. A mechanism that returned "the first account" or "the
    // most recently updated account" would put A's subject here while `iss`
    // still said B — a token that verifies, names a real human, and resolves
    // the wrong grant.
    expect(verifiedB.payload.sub).not.toBe(SUB_A);

    // ---- 6. And A's own session still mints A, from the same user row. -----
    const mintedA = await mintToken(server.origin, flowA.agent);
    expect(mintedA.status).toBe(200);
    const verifiedA = await jwtVerify(String(mintedA.token), jwks, {
      issuer: CONSOLE_ISSUER_A,
      audience: AUDIENCE,
      algorithms: [MOIRA_JWT_ALGORITHM],
    });
    expect(verifiedA.payload.iss).toBe(CONSOLE_ISSUER_A);
    expect(verifiedA.payload.sub).toBe(SUB_A);

    // Two live sessions for ONE human, minting two different (iss, sub) pairs
    // simultaneously. Under `admin_identities`' `(issuer, subject)` key that is
    // TWO grants — the "person-level identity" consequence wave 4 does not
    // ship, made concrete.
    expect(verifiedA.payload.iss).not.toBe(verifiedB.payload.iss);
    expect(verifiedA.payload.sub).not.toBe(verifiedB.payload.sub);
  });
});

/* ========================================================================== */
/* PART 3 — the mutation: remove the stamp, and the mint must REFUSE          */
/* ========================================================================== */

describe("W4-T0 part 3 — an unstamped session must refuse to mint", () => {
  let idpA: MockIdp;
  let idpB: MockIdp;
  let server: SpikeServer;
  /** The memory adapter's backing store, read directly rather than over HTTP. */
  let store: Record<string, unknown[]>;

  beforeAll(async () => {
    useNativeWhatwgGlobals();

    const port = reserveConsolePort();
    const consoleOrigin = `https://localhost:${port}`;
    trustFixtureCa(consoleOrigin);

    idpA = await startMockIdp({
      clientId: CLIENT_ID_A,
      clientSecret: CLIENT_SECRET_A,
      user: { sub: "sub-a", email: "a@corp.test", emailVerified: true, name: "A" },
    });
    idpB = await startMockIdp({
      clientId: CLIENT_ID_B,
      clientSecret: CLIENT_SECRET_B,
      user: { sub: "sub-b", email: "b@contractor.test", emailVerified: true, name: "B" },
    });
    trustFixtureCa(idpA.origin);
    trustFixtureCa(idpB.origin);

    // The mutation: everything identical except the stamping hook is gone.
    // This is also, exactly, the state of every session that predates 4B.
    store = createConsoleMemoryDatabase();
    const auth = createSpikeAuth({
      env: envFor(consoleOrigin),
      providers: providersFor(idpA, idpB),
      database: memoryAdapter(store),
      disableProviderStamp: true,
    });
    server = startSpikeServer(auth, port);
  });

  afterAll(() => {
    server?.stop();
    idpA?.stop();
    idpB?.stop();
    untrustFixtureCa();
    restoreDomWhatwgGlobals();
  });

  test("the column stays null, and the mint refuses rather than defaulting", async () => {
    const { agent, errorParam } = await signIn(server.origin, PROVIDER_B_ID);
    expect(errorParam).toBeNull();

    // Read the persisted row straight out of the adapter's store — not over
    // HTTP, because `/get-session` is itself on the minting path (next test).
    const rows = store["session"] as Record<string, unknown>[];
    expect(rows.length).toBeGreaterThan(0);
    const row = rows.at(-1);
    expect(row?.[SESSION_PROVIDER_FIELD] ?? null).toBeNull();

    // And the mint refuses rather than falling back. This is the constraint 4B
    // inherits: the column is nullable, so pre-4B sessions WILL reach this
    // path, and the only safe behaviour is a refusal. A default would sign a
    // token whose `iss` names a provider that did not authenticate the human.
    const minted = await mintToken(server.origin, agent);
    expect(minted.status).not.toBe(200);
    expect(minted.token).toBeNull();
  });

  test("a refusal that throws also fails /get-session — the coupling 4B inherits", async () => {
    const { agent, errorParam } = await signIn(server.origin, PROVIDER_A_ID);
    expect(errorParam).toBeNull();

    // `dist/plugins/jwt/index.mjs` registers `hooks.after` matching
    // `/get-session` and mints a token into a `set-auth-jwt` header. So
    // `definePayload` runs on the session-read path too, and a refusal that
    // throws takes an ordinary session read down with it.
    //
    // This is NOT the behaviour 4B should ship. Part 4 shows the fix.
    const response = await agent.request(`${server.origin}/api/auth/get-session`);
    expect(response.status).toBe(500);
  });

  test("no token anywhere carries the plugin's fallback issuer", async () => {
    const { agent, errorParam } = await signIn(server.origin, PROVIDER_A_ID);
    expect(errorParam).toBeNull();
    const minted = await mintToken(server.origin, agent);
    // Belt and braces: if a token WERE returned, prove it is not the sentinel.
    // `decodeJwt` rather than `jwtVerify`, because the failure mode under test
    // is a token that verifies perfectly and names the wrong issuer.
    if (minted.token !== null) {
      expect(decodeJwt(minted.token).iss).not.toBe("https://spike.invalid/never-mint-this");
    }
    expect(minted.token).toBeNull();
  });
});

/* ========================================================================== */
/* PART 4 — the shape 4B should actually ship                                 */
/* ========================================================================== */

describe("W4-T0 part 4 — disableSettingJwtHeader decouples refusal from session reads", () => {
  let idpA: MockIdp;
  let idpB: MockIdp;
  let server: SpikeServer;

  beforeAll(async () => {
    useNativeWhatwgGlobals();

    const port = reserveConsolePort();
    const consoleOrigin = `https://localhost:${port}`;
    trustFixtureCa(consoleOrigin);

    idpA = await startMockIdp({
      clientId: CLIENT_ID_A,
      clientSecret: CLIENT_SECRET_A,
      user: { sub: "sub-a4", email: "a4@corp.test", emailVerified: true, name: "A4" },
    });
    idpB = await startMockIdp({
      clientId: CLIENT_ID_B,
      clientSecret: CLIENT_SECRET_B,
      user: { sub: "sub-b4", email: "b4@contractor.test", emailVerified: true, name: "B4" },
    });
    trustFixtureCa(idpA.origin);
    trustFixtureCa(idpB.origin);

    const auth = createSpikeAuth({
      env: envFor(consoleOrigin),
      providers: providersFor(idpA, idpB),
      database: memoryAdapter(createConsoleMemoryDatabase()),
      disableProviderStamp: true,
      disableSettingJwtHeader: true,
    });
    server = startSpikeServer(auth, port);
  });

  afterAll(() => {
    server?.stop();
    idpA?.stop();
    idpB?.stop();
    untrustFixtureCa();
    restoreDomWhatwgGlobals();
  });

  test("an unstamped session reads fine and still cannot mint", async () => {
    const { agent, errorParam } = await signIn(server.origin, PROVIDER_B_ID);
    expect(errorParam).toBeNull();

    // The session read now succeeds — the console's own pages keep working for
    // a human whose session predates 4B.
    const { session } = await readSession(server.origin, agent);
    expect(session[SESSION_PROVIDER_FIELD] ?? null).toBeNull();

    // And minting still refuses. The refusal is where it belongs: at the
    // credential boundary, not on every page load.
    const minted = await mintToken(server.origin, agent);
    expect(minted.status).not.toBe(200);
    expect(minted.token).toBeNull();
  });
});
