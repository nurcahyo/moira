// ============================================================================
// SPIKE — plan 09 wave 4, task T0. THIS IS NOT SHIPPED COVERAGE.
// ============================================================================
//
// The same question as `w4-t0-provider-session.spike.test.ts`, asked on the
// path production actually runs: a durable PostgreSQL database behind Better
// Auth's Kysely adapter, with the provider column added by real DDL rather than
// conjured by the memory adapter.
//
// WHY THIS FILE EXISTS SEPARATELY. `memoryAdapter` stores rows as plain objects
// in an array. It will happily hold a property no schema declares, so "the
// stamp round-trips" is a weaker claim there than it looks. On Kysely the
// column must exist, must be nullable, must be written by the create path and
// must be read back by the select path — four things the memory adapter proves
// none of.
//
// WHICH DATABASE, AND WHY NOT THE USUAL ONE. A DEDICATED database, created by
// this spike:
//
//     console_auth_t0_spike
//
// NOT `console_auth_test`. Other agents are working concurrently against that
// database and against Moira's `moira`, and this file both truncates and adds a
// column. A spike must not be the reason someone else's gate run goes red.
//
// The DDL applied below is not hand-written. It is the verbatim output of
// better-auth's own migration compiler at the pinned version, obtained by
// `tests/spike/w4-t0-derive-0003.ts`, which runs `getMigrations()` against a
// throwaway database that already has `0001` and `0002`:
//
//     alter table "session" add column "providerId" text;
//
// Nullable, by better-auth's own choice, because the field is `required: false`.
// That nullability is the constraint Stage 4B inherits and the reason the last
// test in this file exists.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { createRemoteJWKSet, jwtVerify } from "jose";
import type { Pool } from "pg";

import { MOIRA_JWT_ALGORITHM } from "@/lib/auth";
import { AUTH_BASE_PATH, AUTH_JWKS_PATH, readConsoleEnv } from "@/lib/env";

import { createBrowserAgent } from "../support/browser-agent";
import {
  DATABASE_TESTS_SKIPPED,
  describeDatabase,
  openConsoleTestDatabase,
} from "../support/console-db";
import { reserveConsolePort } from "../support/console-server";
import { fixtureTls, trustFixtureCa, untrustFixtureCa } from "../support/fixture-tls";
import { restoreDomWhatwgGlobals, useNativeWhatwgGlobals } from "../support/native-globals";
import { startMockIdp, type MockIdp } from "../support/mock-idp";

import { createSpikeAuth, SESSION_PROVIDER_FIELD, type SpikeAuth } from "./w4-t0-spike-auth";

/** This spike's own database. Never `console_auth_test`. */
const SPIKE_DSN =
  process.env["CONSOLE_T0_SPIKE_DATABASE_URL"] ??
  "postgres://postgres:postgres@127.0.0.1:5432/console_auth_t0_spike";

/** Verbatim from `tests/spike/w4-t0-derive-0003.ts`. This is candidate `0003`. */
const CANDIDATE_0003 = 'alter table "session" add column "providerId" text;';

const AUDIENCE = "moira-admin-api";
const PROVIDER_A_ID = "moira-console-idp";
const PROVIDER_B_ID = "moira-console-idp-contractors";
const CONSOLE_ISSUER_A = "https://console.test/idp/corp";
const CONSOLE_ISSUER_B = "https://console.test/idp/contractors";
const CLIENT_ID_A = "moira-console-a.apps.mock-idp.test";
const CLIENT_SECRET_A = "mock-idp-a-client-secret-do-not-reuse";
const CLIENT_ID_B = "moira-console-b.apps.mock-idp.test";
const CLIENT_SECRET_B = "mock-idp-b-client-secret-do-not-reuse";

const SHARED_EMAIL = "dual@corp.test";
const SUB_A = "corp-idp-subject-durable-aaaa";
const SUB_B = "contractor-idp-subject-durable-bbbb";

describeDatabase("W4-T0 durable — the stamp on a real PostgreSQL session table", () => {
  let pool: Pool;
  let idpA: MockIdp;
  let idpB: MockIdp;
  let auth: SpikeAuth;
  let origin: string;
  let jwksUrl: string;
  let server: { stop(): void };

  beforeAll(async () => {
    if (DATABASE_TESTS_SKIPPED) return;
    useNativeWhatwgGlobals();

    // Creates the database if absent and applies the committed migrations.
    pool = await openConsoleTestDatabase(SPIKE_DSN);
    // Then the candidate `0003`, idempotently — this file may run repeatedly.
    await pool.query(CANDIDATE_0003.replace("add column", "add column if not exists"));
    // Safe: this database belongs to the spike and to nothing else.
    await pool.query(
      'truncate table console_provider_secret, "jwks", "rateLimit", "session", "account", ' +
        '"verification", "user" cascade',
    );

    const port = reserveConsolePort();
    origin = `https://localhost:${port}`;
    trustFixtureCa(origin);

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
      env: readConsoleEnv({
        NODE_ENV: "test",
        MOIRA_API_URL: "https://moira.invalid",
        CONSOLE_PUBLIC_ORIGIN: origin,
        MOIRA_ADMIN_API_AUDIENCE: AUDIENCE,
        BETTER_AUTH_SECRET: "fixture-better-auth-secret-at-least-32-chars",
        CONSOLE_SECRET_ENCRYPTION_KEY: Buffer.alloc(32, 7).toString("base64"),
      }),
      providers: [
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
      ],
      // The console's own `pg.Pool`, exactly as `lib/auth-runtime.ts` supplies
      // it: `createKyselyAdapter` recognises it by its `connect` method.
      database: pool,
    });

    const tls = fixtureTls();
    const bunServer = Bun.serve({
      port,
      hostname: "127.0.0.1",
      tls: { key: tls.key, cert: tls.cert },
      async fetch(request): Promise<Response> {
        const url = new URL(request.url);
        if (url.pathname.startsWith(AUTH_BASE_PATH)) return auth.handler(request);
        return new Response("console", { status: 200 });
      },
    });
    jwksUrl = `${origin}${AUTH_BASE_PATH}${AUTH_JWKS_PATH}`;
    server = { stop: () => bunServer.stop(true) };
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

  async function signIn(providerId: string): Promise<ReturnType<typeof createBrowserAgent>> {
    const agent = createBrowserAgent();
    const start = await agent.request(`${origin}/api/auth/sign-in/oauth2`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ providerId, callbackURL: "/" }),
    });
    expect(start.status).toBe(200);
    const { url } = (await start.json()) as { url: string };
    await agent.navigate(url);
    for (const hop of agent.hops) {
      if (hop.location === null) continue;
      const error = new URL(hop.location, hop.url).searchParams.get("error");
      expect(error).toBeNull();
    }
    return agent;
  }

  async function mint(
    agent: ReturnType<typeof createBrowserAgent>,
  ): Promise<{ status: number; token: string | null }> {
    const response = await agent.request(`${origin}/api/auth/token`);
    const body = (await response.json().catch(() => null)) as { token?: unknown } | null;
    return {
      status: response.status,
      token: typeof body?.token === "string" ? body.token : null,
    };
  }

  test("G10 on PostgreSQL: two linked accounts, and the SQL row names the right one", async () => {
    // ---- Provider A creates the user. --------------------------------------
    const agentA = await signIn(PROVIDER_A_ID);
    // ---- Provider B links onto it, by verified email. ----------------------
    const agentB = await signIn(PROVIDER_B_ID);

    // The precondition, asserted in SQL rather than through the library: one
    // user row, two account rows.
    const users = await pool.query<{ id: string }>('select id from "user"');
    expect(users.rowCount).toBe(1);
    const accounts = await pool.query<{ providerId: string; accountId: string }>(
      'select "providerId", "accountId" from "account" order by "providerId"',
    );
    expect(accounts.rows.map((row) => row.providerId)).toEqual([PROVIDER_A_ID, PROVIDER_B_ID]);
    expect(accounts.rows.map((row) => row.accountId).sort()).toEqual([SUB_A, SUB_B].sort());

    // Two sessions, each carrying its own provider — in the actual column.
    const sessions = await pool.query<{ providerId: string | null }>(
      `select "${SESSION_PROVIDER_FIELD}" from "session" order by "createdAt"`,
    );
    expect(sessions.rows.map((row) => row.providerId)).toEqual([PROVIDER_A_ID, PROVIDER_B_ID]);

    // ---- And the tokens. ---------------------------------------------------
    const jwks = createRemoteJWKSet(new URL(jwksUrl));

    const tokenB = await mint(agentB);
    expect(tokenB.status).toBe(200);
    const verifiedB = await jwtVerify(String(tokenB.token), jwks, {
      issuer: CONSOLE_ISSUER_B,
      audience: AUDIENCE,
      algorithms: [MOIRA_JWT_ALGORITHM],
    });
    expect(verifiedB.payload.iss).toBe(CONSOLE_ISSUER_B);
    expect(verifiedB.payload.sub).toBe(SUB_B);

    const tokenA = await mint(agentA);
    expect(tokenA.status).toBe(200);
    const verifiedA = await jwtVerify(String(tokenA.token), jwks, {
      issuer: CONSOLE_ISSUER_A,
      audience: AUDIENCE,
      algorithms: [MOIRA_JWT_ALGORITHM],
    });
    expect(verifiedA.payload.iss).toBe(CONSOLE_ISSUER_A);
    expect(verifiedA.payload.sub).toBe(SUB_A);

    // One human, two simultaneous grants, distinguished only by `iss`.
    expect(verifiedA.payload.iss).not.toBe(verifiedB.payload.iss);
  });

  test("a session that predates the column refuses to mint, rather than defaulting", async () => {
    const agent = await signIn(PROVIDER_A_ID);
    expect((await mint(agent)).status).toBe(200);

    // Now make it look exactly like a session created before `0003` shipped.
    // This is the upgrade case: `0003` adds a nullable column, so every session
    // live at deploy time has NULL in it.
    const nulled = await pool.query(
      `update "session" set "${SESSION_PROVIDER_FIELD}" = null ` +
        `where "${SESSION_PROVIDER_FIELD}" = $1`,
      [PROVIDER_A_ID],
    );
    expect(nulled.rowCount).toBeGreaterThan(0);

    const after = await mint(agent);
    expect(after.status).not.toBe(200);
    expect(after.token).toBeNull();
  });
});
