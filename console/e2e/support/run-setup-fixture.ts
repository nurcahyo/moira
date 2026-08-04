// The setup-wizard fixture stack, as one process Playwright can start and kill.
//
// Started by `playwright.config.ts`'s SECOND `webServer` entry. Playwright runs
// `webServer` entries in ORDER (each plugin's `setup` is awaited before the next
// one starts), so by the time this runs the first entry has already produced
// `.next/standalone` and is serving the main e2e console. That ordering is what
// lets this file start a console server without building one.
//
// What it does, in order:
//
//   1. creates and migrates the fixture database (`console_setup_e2e`);
//   2. starts the stub Moira on TLS;
//   3. spawns the SHIPPED standalone server with the fixture environment;
//   4. stays alive until it is killed, then takes the child with it.
//
// It is a `bun` script rather than a Node one so it can import the repository's
// TypeScript directly — `db/migrate.ts` in particular, so the fixture database
// is migrated by the same code `bun run db:migrate` runs and cannot drift from
// it.

import { spawn, type ChildProcess } from "node:child_process";
import path from "node:path";

import { Pool } from "pg";

import { redactDsn, splitDatabase } from "../../db/dsn";
import { applyMigrations, loadMigrations } from "../../db/migrate";
import { CONSOLE_ROOT } from "./paths";
import { startMoiraSetupStub } from "./moira-setup-stub";
import { SENTINEL_ENV } from "./secrets";
import {
  readSetupFixtureTls,
  setupFixtureDatabaseUrl,
  setupFixtureTls,
  SETUP_FIXTURE_CONSOLE_PORT,
  SETUP_FIXTURE_MOIRA_PORT,
  SETUP_FIXTURE_MOIRA_URL,
  SETUP_FIXTURE_PUBLIC_ORIGIN,
  SETUP_FIXTURE_SYSTEM_KEY,
} from "./setup-fixture";

/* -------------------------------------------------------------------------- */
/* 1. the fixture database                                                    */
/* -------------------------------------------------------------------------- */

/**
 * Create the fixture database if it is not there, then migrate it.
 *
 * `42P04` (duplicate_database) is the one error worth swallowing; everything
 * else is reported with the DSN redacted, because a connection string carries
 * its password inline and this output goes to CI logs.
 */
async function prepareDatabase(dsn: string): Promise<void> {
  const { admin, database } = splitDatabase(dsn);
  const adminPool = new Pool({ connectionString: admin, max: 1, connectionTimeoutMillis: 5_000 });
  try {
    await adminPool.query(`create database "${database}"`);
  } catch (error) {
    if ((error as { code?: string }).code !== "42P04") {
      throw new Error(
        `cannot reach PostgreSQL at ${redactDsn(admin)}: ${(error as Error).message}\n` +
          "The setup-wizard e2e fixture needs a real console database: the wizard's sign-in " +
          "step is gated on `consoleSecretStored`, which `deriveProvisioningState` answers " +
          "from the console's own secret store, and a production-shaped console has no " +
          "in-memory store. Start PostgreSQL, or point CONSOLE_TEST_DATABASE_URL / " +
          "CONSOLE_SETUP_E2E_DATABASE_URL at one.",
      );
    }
  } finally {
    await adminPool.end();
  }

  const pool = new Pool({ connectionString: dsn, max: 1, connectionTimeoutMillis: 5_000 });
  pool.on("error", () => {});
  try {
    await applyMigrations(pool, loadMigrations());
    // A fresh wizard run every time. The suite provisions a provider and seals
    // its client secret; leaving those behind would make the second run start
    // from a state the first one wrote.
    await pool.query("truncate table console_provider_secret cascade");
  } finally {
    await pool.end();
  }
}

/* -------------------------------------------------------------------------- */
/* 3. the fixture console                                                     */
/* -------------------------------------------------------------------------- */

function startFixtureConsole(databaseUrl: string, caCertPath: string): ChildProcess {
  const child = spawn("node", [path.join(CONSOLE_ROOT, ".next", "standalone", "server.js")], {
    cwd: CONSOLE_ROOT,
    stdio: ["ignore", "inherit", "inherit"],
    env: {
      ...process.env,
      PORT: String(SETUP_FIXTURE_CONSOLE_PORT),
      HOSTNAME: "127.0.0.1",
      // Next's standalone entrypoint sets this itself; stated here so the
      // fixture's shape is legible without reading the generated server.
      NODE_ENV: "production",
      CONSOLE_PUBLIC_ORIGIN: SETUP_FIXTURE_PUBLIC_ORIGIN,
      MOIRA_API_URL: SETUP_FIXTURE_MOIRA_URL,
      // Pinned against a developer's `.env.local`, which Next also loads in the
      // standalone runtime: `scripts/dev-env.sh` writes this flag, and
      // `lib/env.ts` refuses it under NODE_ENV=production, so the fixture
      // console would fail to boot on exactly the machines that have one. Every
      // other variable this process needs is set explicitly below or above.
      CONSOLE_ALLOW_INSECURE_URLS: "false",
      // The difference from the main e2e console, and the whole point of this
      // fixture: with a bootstrap key present, `withSetupWindow` opens.
      MOIRA_SYSTEM_KEY: SETUP_FIXTURE_SYSTEM_KEY,
      CONSOLE_DATABASE_URL: databaseUrl,
      // Scoped trust for the stub's self-signed certificate. NOT
      // `NODE_TLS_REJECT_UNAUTHORIZED=0`, which would switch chain validation
      // off for every origin this process talks to.
      NODE_EXTRA_CA_CERTS: caCertPath,
      // Sentinels travel with the fixture console too, so the leak scans in
      // `setup-wizard.e2e.ts` are looking for values this server actually holds.
      ...SENTINEL_ENV,
    },
  });
  return child;
}

/* -------------------------------------------------------------------------- */
/* main                                                                       */
/* -------------------------------------------------------------------------- */

async function main(): Promise<void> {
  const databaseUrl = setupFixtureDatabaseUrl();
  await prepareDatabase(databaseUrl);

  const tlsPaths = setupFixtureTls();
  const tls = readSetupFixtureTls();
  const stub = await startMoiraSetupStub({
    port: SETUP_FIXTURE_MOIRA_PORT,
    cert: tls.cert,
    key: tls.key,
    systemKey: SETUP_FIXTURE_SYSTEM_KEY,
  });
  process.stdout.write(`setup fixture: stub Moira on ${SETUP_FIXTURE_MOIRA_URL}\n`);

  const child = startFixtureConsole(databaseUrl, tlsPaths.certPath);
  process.stdout.write(
    `setup fixture: console on http://127.0.0.1:${SETUP_FIXTURE_CONSOLE_PORT}\n`,
  );

  let shuttingDown = false;
  const shutdown = (): void => {
    if (shuttingDown) return;
    shuttingDown = true;
    child.kill("SIGTERM");
    void stub.close().then(() => process.exit(0));
  };
  process.on("SIGTERM", shutdown);
  process.on("SIGINT", shutdown);
  child.on("exit", (code) => {
    if (!shuttingDown) {
      process.stderr.write(`setup fixture: console exited with code ${String(code)}\n`);
      void stub.close().then(() => process.exit(code ?? 1));
    }
  });
}

void main().catch((error: unknown) => {
  process.stderr.write(`setup fixture failed to start: ${String(error)}\n`);
  process.exit(1);
});
