// The AUTHENTICATED e2e stack: everything a browser needs in order to actually
// sign in to this console, started as one process.
//
// Run by `playwright.config.ts` as the second `webServer` entry:
//
//   bun --preload ./tests/support/server-only-runtime-shim.ts \
//       e2e/support/authenticated-stack.ts
//
// The preload is not optional. `lib/console-secrets-postgres.ts` carries
// `import "server-only"`, whose `default` export condition is a bare `throw`;
// the shim is the same neutralisation `tests/support/durability-probe.ts`
// already uses for a plain `bun` process, and `server-only-runtime-shim.ts`'s
// own header explains why it does not weaken the guard.
//
// ============================================================================
// WHAT IS REAL HERE
// ============================================================================
//
//   * the SAME standalone artifact the scaffold's `webServer` serves and the
//     Dockerfile ships — `node .next/standalone/server.js`, `NODE_ENV=production`;
//   * a real OpenID Connect provider on a real TLS socket, reused verbatim from
//     `tests/support/mock-idp.ts`;
//   * the console's own PostgreSQL database, migrated and truncated, so Better
//     Auth's session, account and jwks tables are the shipped ones;
//   * a real OpenAI-compatible endpoint on a real socket;
//   * a TLS terminator in front of the console.
//
// MOCKED: Moira's HTTP surface (`moira-fixture.ts`), and the identity provider's
// *identity*. Both are stated where they are built.
//
// ============================================================================
// WHY THE CONSOLE IS BEHIND A TLS PROXY RATHER THAN SPOKEN TO DIRECTLY
// ============================================================================
//
// `.next/standalone/server.js` sets `process.env.NODE_ENV = 'production'` on line
// 11, unconditionally. Under production `lib/env.ts` refuses `http://` origins
// and makes `CONSOLE_ALLOW_INSECURE_URLS` a hard boot failure — deliberately,
// because the console's `jwks_url` is validated by Moira's SSRF policy and a
// console that allowed itself an insecure origin would register a URL Moira must
// refuse anyway.
//
// So `CONSOLE_PUBLIC_ORIGIN` has to be an https origin that the BROWSER can also
// reach, and Next's standalone server terminates no TLS. A terminator in front of
// it is not a fudge: `playwright.config.ts` already records that a real console
// "runs behind a TLS-terminating ingress ... so the origin it advertises as its
// issuer and JWKS host is never the address it binds". This stack is that
// sentence, made executable.
//
// ============================================================================
// IT WAITS FOR THE SCAFFOLD'S SERVER BEFORE SPAWNING ITS OWN
// ============================================================================
//
// Playwright starts `webServer` entries in parallel, and entry 0 is the one that
// runs `bun run build` and assembles `.next/standalone`. Two Next builds writing
// one `.next` concurrently is a corrupted output, so this entry BUILDS NOTHING
// and instead waits until entry 0 answers — which is the only observable proof
// that the build finished and the static assets were copied in.

import { Pool } from "pg";

import { applyMigrations, loadMigrations } from "../../db/migrate";
import { redactDsn, splitDatabase } from "../../db/dsn";
import { PostgresConsoleSecretStore } from "../../lib/console-secrets-postgres";
import { LOCAL_VLLM_BASE_URL } from "../../lib/llm-view";
import { fixtureTls } from "../../tests/support/fixture-tls";
import { startMockIdp } from "../../tests/support/mock-idp";
import {
  AUTH_ADMIN_API_AUDIENCE,
  AUTH_ALLOWED_EMAIL_DOMAIN,
  AUTH_CLIENT_ID,
  AUTH_CLIENT_SECRET,
  AUTH_CONSOLE_INTERNAL_PORT,
  AUTH_CONSOLE_ORIGIN,
  AUTH_CONSOLE_PORT,
  AUTH_CONTROL_PORT,
  AUTH_OPERATOR,
  MOIRA_IDS,
  type FixtureReport,
} from "./authenticated-fixture";
import { FIXED_PORTS, SCAFFOLD_CONSOLE_PORT, assertFixedPortsAreFree } from "./fixed-ports";
import { createMoiraFixture, discoveryBody } from "./moira-fixture";

/* -------------------------------------------------------------------------- */
/* Fixture configuration                                                      */
/* -------------------------------------------------------------------------- */

/**
 * The SERVER `tests/support/console-db.ts` uses, and the same rule as that file:
 * NO SILENT SKIP. An unreachable database is a loud failure, because a console
 * running on in-memory storage cannot keep the session this whole project exists
 * to obtain.
 */
const TEST_DATABASE_URL =
  process.env["CONSOLE_TEST_DATABASE_URL"] ??
  "postgres://postgres:postgres@127.0.0.1:5432/console_auth_test";

/**
 * ============================================================================
 * A SEPARATE DATABASE FROM `bun test`'s — REPRODUCED, NOT ASSUMED
 * ============================================================================
 *
 * The first version of this file pointed straight at `CONSOLE_TEST_DATABASE_URL`
 * and seeded the console's OAuth client secret into it. The next `bun test` then
 * failed:
 *
 *   console-secret-store-postgres.test.ts > read and reveal return null for an
 *   unknown provider — expect(await store().newestUpdatedAt()).toBeNull()
 *   Received: "2026-08-05T00:16:17.471Z"
 *
 * That row was this stack's. The unit suite truncates between FILES; it cannot
 * defend against another process writing to its database in between runs, and
 * the gate order (`bun test` then `bun run e2e`) means the damage always lands
 * on the NEXT run — which is the worst possible shape for it, because the change
 * that caused it is already merged by then.
 *
 * So the two get their own databases, for exactly the reason
 * `tests/support/console-db.ts` gives for keeping the console's separate from
 * Moira's: "two independent migration ledgers in one database is the failure this
 * default exists to make impossible to reach by accident". Created on demand,
 * on the same server, so nothing extra has to be provisioned in CI.
 */
const DATABASE_URL = ((): string => {
  const parsed = new URL(TEST_DATABASE_URL);
  const name = parsed.pathname.replace(/^\//, "");
  parsed.pathname = `/${name === "" ? "console_auth" : name}_playwright`;
  return parsed.href;
})();

/** Fixture values. Every one is public and none is a deployment secret. */
const CONSOLE_ENV = {
  MOIRA_ADMIN_API_AUDIENCE: AUTH_ADMIN_API_AUDIENCE,
  BETTER_AUTH_SECRET: "moira-console-authenticated-e2e-better-auth-secret-4b19",
  // 32 bytes, base64. It seals ONE row — the fixture OAuth client secret — into
  // the throwaway `*_playwright` database this stack truncates on every run, and
  // the same process hands it to the console child that unseals it.
  //
  // It is BASELINED in `.gitleaksignore`, under its own heading, with the reason
  // it is a constant rather than a per-run random value. An earlier version of
  // this comment claimed the alphabet avoided `+` and `/` "for the same reason
  // console-env.ts gives" — that a needle containing `/` is discarded by the
  // secret-leak harness. That reason does not apply here: `forbiddenValues()`
  // harvests the PLAYWRIGHT RUNNER's ambient environment, and this value never
  // enters it — it is composed in this file and passed to a spawned child. The
  // secret-leak suite runs against the scaffold server, not this stack.
  CONSOLE_SECRET_ENCRYPTION_KEY: "gTdKq2mBcXWvPzR8sLnY4hJeA6uF0iOaQwErTyUiOpA=",
  // The bootstrap credential. `consoleRuntime()` cannot resolve an auth
  // configuration without one on a cold process — see `lib/auth-runtime.ts`.
  MOIRA_SYSTEM_KEY: "sk_authenticated_e2e_bootstrap_2f81cd",
} as const;

/** Tables `resetConsoleTestDatabase` truncates, in the same order. */
const CONSOLE_TABLES =
  'console_provider_secret, "jwks", "rateLimit", "session", "account", "verification", "user"';

/* -------------------------------------------------------------------------- */
/* Small helpers                                                              */
/* -------------------------------------------------------------------------- */

function log(message: string): void {
  process.stdout.write(`[authenticated-stack] ${message}\n`);
}

async function sleep(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

/** Poll until `check` succeeds, or fail with `what` named. */
async function waitFor(what: string, timeoutMs: number, check: () => Promise<boolean>) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    if (await check()) return;
    if (Date.now() > deadline)
      throw new Error(`timed out after ${timeoutMs} ms waiting for ${what}`);
    await sleep(250);
  }
}

async function answers(url: string): Promise<boolean> {
  try {
    await fetch(url, { method: "GET", redirect: "manual" });
    return true;
  } catch {
    return false;
  }
}

/* -------------------------------------------------------------------------- */
/* Database                                                                   */
/* -------------------------------------------------------------------------- */

/**
 * A migrated, empty console database, and the OAuth client secret seeded into
 * it.
 *
 * The seed is what makes the deployment signable-in-to at all: `resolveAuthConfigs`
 * refuses a provider row whose sealed secret is missing or whose `client_id` has
 * drifted (`console_secret_unavailable`), and it writes through the SHIPPED
 * `PostgresConsoleSecretStore` so the envelope this fixture stores is the
 * envelope the console reads.
 */
async function prepareDatabase(): Promise<Pool> {
  const { admin, database } = splitDatabase(DATABASE_URL);
  const adminPool = new Pool({ connectionString: admin, max: 1, connectionTimeoutMillis: 5_000 });
  try {
    await adminPool.query(`create database "${database}"`);
  } catch (error) {
    if ((error as { code?: string }).code !== "42P04") {
      throw new Error(
        `cannot reach PostgreSQL at ${redactDsn(DATABASE_URL)}: ${(error as Error).message}\n` +
          "The authenticated e2e stack needs a real database: `.next/standalone/server.js` sets " +
          "NODE_ENV=production, and lib/env.ts refuses to boot production without one. Start " +
          "PostgreSQL, or point CONSOLE_TEST_DATABASE_URL at one.",
      );
    }
  } finally {
    await adminPool.end();
  }

  const pool = new Pool({ connectionString: DATABASE_URL, max: 4, connectionTimeoutMillis: 5_000 });
  pool.on("error", () => {});
  await applyMigrations(pool, loadMigrations());
  // A run starts from no sessions and no signing key, so the sign-in the setup
  // project performs is a real first sign-in rather than a reused cookie.
  await pool.query(`truncate table ${CONSOLE_TABLES} cascade`);

  const store = new PostgresConsoleSecretStore(
    pool,
    Buffer.from(CONSOLE_ENV.CONSOLE_SECRET_ENCRYPTION_KEY, "base64"),
  );
  await store.put(MOIRA_IDS.authProvider, AUTH_CLIENT_ID, AUTH_CLIENT_SECRET);
  return pool;
}

/* -------------------------------------------------------------------------- */
/* Main                                                                       */
/* -------------------------------------------------------------------------- */

const stoppers: Array<() => void> = [];

function shutdown(code: number): never {
  for (const stop of stoppers.reverse()) {
    try {
      stop();
    } catch {
      /* a stack coming down must not fail on the way */
    }
  }
  process.exit(code);
}

/**
 * The three fixed ports THIS process binds — not the scaffold's, which is
 * expected to be occupied by the time this runs (entry 0 owns the build, and
 * step 0 below waits for it to answer).
 */
const OWN_FIXED_PORTS = FIXED_PORTS.filter((claim) => claim.port !== SCAFFOLD_CONSOLE_PORT);

async function main(): Promise<void> {
  // ---- 0. the ports are ours, checked before anything is started -----------
  //
  // Before the database, the IdP and a five-minute build wait: an EADDRINUSE
  // raised at step 5, after all of that, reads as a broken stack rather than as
  // a taken port. See `fixed-ports.ts` for the collision this exists for.
  await assertFixedPortsAreFree(OWN_FIXED_PORTS);

  // ---- 1. wait for the scaffold's server, i.e. for the build ---------------
  const scaffoldPort = SCAFFOLD_CONSOLE_PORT;
  if (process.env["CONSOLE_E2E_BASE_URL"] === undefined) {
    log(`waiting for the scaffold server on :${scaffoldPort} (it owns the build)`);
    await waitFor("the scaffold e2e server", 300_000, () =>
      answers(`http://127.0.0.1:${scaffoldPort}/login`),
    );
  }
  if (!(await Bun.file(".next/standalone/server.js").exists())) {
    throw new Error(
      ".next/standalone/server.js is missing. This entry deliberately builds nothing — the " +
        "scaffold `webServer` owns `bun run build`, because two Next builds writing one .next " +
        "concurrently is a corrupted output.",
    );
  }

  // ---- 2. TLS, the identity provider, and the endpoint --------------------
  const tls = fixtureTls();

  const idp = await startMockIdp({
    clientId: AUTH_CLIENT_ID,
    clientSecret: AUTH_CLIENT_SECRET,
    user: AUTH_OPERATOR,
  });
  stoppers.push(() => idp.stop());
  log(`mock IdP at ${idp.issuer}`);

  let vllmRequests: string[] = [];
  let vllmReachable = true;
  const vllm = Bun.serve({
    port: 0,
    hostname: "127.0.0.1",
    fetch(request) {
      const url = new URL(request.url);
      vllmRequests.push(`${request.method} ${url.pathname}`);
      if (!vllmReachable) return new Response("gone", { status: 503 });
      if (url.pathname === "/v1/models") {
        return new Response(JSON.stringify(discoveryBody()), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      return new Response("not found", { status: 404 });
    },
  });
  stoppers.push(() => vllm.stop(true));
  const vllmOrigin = `http://127.0.0.1:${vllm.port}`;
  const vllmBaseUrl = `${vllmOrigin}/v1`;

  // ======================================================================
  // THE FIXTURE ENDPOINT IS A SOCKET THIS PROCESS BOUND — ASSERTED, NOT HOPED
  // ======================================================================
  //
  // The rule this whole harness is written under: a test must never probe the
  // real deployment named by the field's pre-filled default.
  //
  // The first version of this guard compared `vllmOrigin` against
  // `LOCAL_VLLM_BASE_URL` for equality. It could not fail. `vllmOrigin` is built
  // two lines above as `http://127.0.0.1:${vllm.port}` from a port the OS just
  // assigned, and `LOCAL_VLLM_BASE_URL` is `https://local-llm.motrait.com`; the
  // two differ in scheme, host and port on every possible run, so the guard was
  // a comment with an `if` around it.
  //
  // What is actually worth asserting is the POSITIVE property the equality was
  // standing in for: the address the specs will be handed is loopback, and it is
  // the port `Bun.serve` reported. That fails the moment an edit points the
  // fixture at any host this process did not bind — including, but not limited
  // to, the shipped default.
  const parsedVllm = new URL(vllmBaseUrl);
  if (parsedVllm.hostname !== "127.0.0.1" || parsedVllm.port !== String(vllm.port)) {
    throw new Error(
      `the fixture endpoint resolved to ${vllmBaseUrl}, which is not the loopback socket this ` +
        `process bound (127.0.0.1:${vllm.port}). The authenticated suite must discover from a ` +
        `mock it started — never from the shipped default (${LOCAL_VLLM_BASE_URL}) or any other ` +
        "host that might be a real deployment.",
    );
  }
  log(`mock OpenAI-compatible endpoint at ${vllmBaseUrl}`);

  // ---- 3. the fixture Moira, on TLS ---------------------------------------
  const moira = createMoiraFixture({
    idp: {
      issuer: idp.issuer,
      discoveryUrl: idp.discoveryUrl,
      authorizationUrl: idp.authorizationUrl,
      tokenUrl: idp.tokenUrl,
      userInfoUrl: idp.userInfoUrl,
    },
    clientId: AUTH_CLIENT_ID,
    allowedEmailDomain: AUTH_ALLOWED_EMAIL_DOMAIN,
    adminApiAudience: AUTH_ADMIN_API_AUDIENCE,
    consoleIssuer: AUTH_CONSOLE_ORIGIN,
    consoleJwksUrl: `${AUTH_CONSOLE_ORIGIN}/api/auth/.well-known/jwks.json`,
  });
  const moiraServer = Bun.serve({
    port: 0,
    hostname: "127.0.0.1",
    tls: { key: tls.key, cert: tls.cert },
    fetch: (request) => moira.handle(request),
  });
  stoppers.push(() => moiraServer.stop(true));
  const moiraBaseUrl = `https://localhost:${moiraServer.port}`;
  log(`fixture Moira at ${moiraBaseUrl}`);

  // ---- 4. the database -----------------------------------------------------
  const pool = await prepareDatabase();
  stoppers.push(() => {
    void pool.end();
  });
  log(`console database ready at ${redactDsn(DATABASE_URL)}`);

  // ---- 5. the console itself ----------------------------------------------
  const child = Bun.spawn({
    cmd: ["node", ".next/standalone/server.js"],
    stdout: "inherit",
    stderr: "inherit",
    env: {
      PATH: process.env["PATH"] ?? "",
      HOME: process.env["HOME"] ?? "",
      PORT: String(AUTH_CONSOLE_INTERNAL_PORT),
      HOSTNAME: "127.0.0.1",
      NODE_ENV: "production",
      // Node reads this ONCE, at process start — which is exactly why the
      // fixture CA is passed to the child rather than installed with a `fetch`
      // wrapper the way `trustFixtureCa` does it for an in-process suite. The
      // console's outbound calls to the fixture Moira and to the IdP get REAL
      // chain validation against a known CA; nothing here disables it.
      NODE_EXTRA_CA_CERTS: tls.certPath,
      MOIRA_API_URL: moiraBaseUrl,
      CONSOLE_PUBLIC_ORIGIN: AUTH_CONSOLE_ORIGIN,
      CONSOLE_DATABASE_URL: DATABASE_URL,
      ...CONSOLE_ENV,
    },
  });
  stoppers.push(() => child.kill());

  const internalOrigin = `http://127.0.0.1:${AUTH_CONSOLE_INTERNAL_PORT}`;
  await waitFor("the authenticated console to boot", 120_000, () =>
    answers(`${internalOrigin}/login`),
  );
  log(`console listening internally on ${internalOrigin}`);

  // ---- 6. TLS termination, started only once the console answers -----------
  //
  // Order matters: Playwright treats this port answering as "the stack is up",
  // so it must not answer before there is something behind it.
  const proxy = Bun.serve({
    port: AUTH_CONSOLE_PORT,
    hostname: "127.0.0.1",
    tls: { key: tls.key, cert: tls.cert },
    async fetch(request) {
      const incoming = new URL(request.url);
      const target = `${internalOrigin}${incoming.pathname}${incoming.search}`;

      // ====================================================================
      // A TERMINATOR MUST NOT RE-ADVERTISE AN ENCODING IT ALREADY UNDID
      // ====================================================================
      //
      // A browser sends `accept-encoding: gzip, deflate, br`; Next honours it
      // and answers `content-encoding: gzip`; `fetch` DECOMPRESSES the body
      // transparently but leaves that header on the `Response`. Forwarding it
      // unchanged tells the browser to gunzip plain bytes, and the navigation
      // hangs rather than failing — which is what this proxy did on its first
      // run, and reads as a TLS or a Next problem rather than a proxy one.
      //
      // Asking upstream for `identity` removes the whole class: there is then
      // no encoding to strip, no length to recompute, and the bytes the
      // browser receives are the bytes Next produced.
      const forwarded = new Headers(request.headers);
      forwarded.set("accept-encoding", "identity");
      // Hop-by-hop, and meaningless across a new connection.
      for (const header of ["connection", "keep-alive", "upgrade", "transfer-encoding"]) {
        forwarded.delete(header);
      }

      // `redirect: "manual"` so the 302 chain of an OAuth callback reaches the
      // browser intact.
      const upstream = await fetch(target, {
        method: request.method,
        headers: forwarded,
        ...(request.method === "GET" || request.method === "HEAD"
          ? {}
          : { body: await request.arrayBuffer() }),
        redirect: "manual",
      });

      // Rebuilt rather than returned as-is, so `content-length` cannot describe
      // a body that is no longer that length. Every other header — including
      // each `Set-Cookie`, which is why `Headers` is copied rather than
      // flattened into an object — survives.
      const headers = new Headers(upstream.headers);
      headers.delete("content-encoding");
      headers.delete("content-length");
      return new Response(upstream.body, {
        status: upstream.status,
        statusText: upstream.statusText,
        headers,
      });
    },
  });
  stoppers.push(() => proxy.stop(true));
  log(`console reachable at ${AUTH_CONSOLE_ORIGIN}`);

  // ---- 7. the control surface a spec drives the fixture through -----------
  const control = Bun.serve({
    port: AUTH_CONTROL_PORT,
    hostname: "127.0.0.1",
    fetch(request) {
      const url = new URL(request.url);
      const method = request.method.toUpperCase();

      if (url.pathname === "/fixture" && method === "DELETE") {
        moira.reset();
        vllmRequests = [];
        vllmReachable = true;
        return new Response(null, { status: 204 });
      }
      if (url.pathname === "/recording" && method === "DELETE") {
        moira.forgetRecording();
        vllmRequests = [];
        return new Response(null, { status: 204 });
      }
      if (url.pathname === "/fixture" && method === "GET") {
        const report: FixtureReport = {
          vllmBaseUrl,
          vllmOrigin,
          moiraBaseUrl,
          idpIssuer: idp.issuer,
          moiraRequests: moira.recording(),
          vllmRequests,
          rows: moira.rows(),
        };
        return new Response(JSON.stringify(report), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      return new Response("not found", { status: 404 });
    },
  });
  stoppers.push(() => control.stop(true));
  log(`control surface at http://127.0.0.1:${AUTH_CONTROL_PORT}`);

  // Playwright kills the process group; both signals arrive here.
  process.on("SIGTERM", () => shutdown(0));
  process.on("SIGINT", () => shutdown(0));
  // If the console dies, take the stack down rather than leaving a proxy in
  // front of nothing — a 502 loop is a much worse diagnostic than a dead port.
  void child.exited.then((code) => {
    log(`the console exited with ${code}`);
    shutdown(code === 0 ? 0 : 1);
  });
}

main().catch((error: unknown) => {
  process.stderr.write(`[authenticated-stack] ${String(error)}\n`);
  shutdown(1);
});
