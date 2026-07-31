// The wiring: does the process actually USE the durable store it configured?
//
// `console-secret-store-postgres.test.ts` proves the store works. This file
// proves it is the one `lib/auth-runtime.ts` hands out — the gap between those
// two is a class of bug that leaves every other test green while production
// quietly runs on a `Map`.
import { afterAll, afterEach, beforeAll, expect, test } from "bun:test";
import { randomBytes } from "node:crypto";

import { Pool } from "pg";

import {
  consoleSecretStore,
  consoleStorageMode,
  resetConsoleRuntime,
} from "../../lib/auth-runtime";
import { closeConsoleDatabase, hasConsoleDatabase } from "../../lib/console-db";
import { InMemoryConsoleSecretStore } from "../../lib/console-secrets";
import { PostgresConsoleSecretStore } from "../../lib/console-secrets-postgres";
import { readConsoleEnv, type ConsoleEnv, type EnvSource } from "../../lib/env";
import {
  DATABASE_TESTS_SKIPPED,
  describeDatabase,
  openConsoleTestDatabase,
  resetConsoleTestDatabase,
  testDatabaseUrl,
} from "../support/console-db";

const BASE: EnvSource = {
  NODE_ENV: "test",
  MOIRA_API_URL: "https://moira.invalid",
  CONSOLE_PUBLIC_ORIGIN: "https://console.invalid",
  MOIRA_ADMIN_API_AUDIENCE: "moira-admin",
  BETTER_AUTH_SECRET: "a-secret-that-is-at-least-32-characters",
  CONSOLE_SECRET_ENCRYPTION_KEY: randomBytes(32).toString("base64"),
};

function envWithDatabase(): ConsoleEnv {
  return readConsoleEnv({ ...BASE, CONSOLE_DATABASE_URL: testDatabaseUrl() });
}

let pool: Pool;

beforeAll(async () => {
  if (DATABASE_TESTS_SKIPPED) return;
  pool = await openConsoleTestDatabase();
});

afterEach(async () => {
  // Both singletons, or the next test inherits this one's decision.
  resetConsoleRuntime();
  await closeConsoleDatabase();
  if (DATABASE_TESTS_SKIPPED) return;
  await resetConsoleTestDatabase(pool);
});

afterAll(async () => {
  await pool?.end();
});

describeDatabase("storage selection", () => {
  test("a configured database yields the durable store", () => {
    const env = envWithDatabase();
    expect(hasConsoleDatabase(env)).toBe(true);
    expect(consoleStorageMode(env)).toBe("durable");
    expect(consoleSecretStore(env)).toBeInstanceOf(PostgresConsoleSecretStore);
  });

  test("no database yields the in-memory store", () => {
    const env = readConsoleEnv(BASE);
    expect(hasConsoleDatabase(env)).toBe(false);
    expect(consoleStorageMode(env)).toBe("ephemeral");
    expect(consoleSecretStore(env)).toBeInstanceOf(InMemoryConsoleSecretStore);
  });

  test("asking whether storage is durable does not open a connection", async () => {
    // `hasConsoleDatabase` exists so a health check or a startup log can answer
    // the question without a socket. If it started constructing a pool, the
    // answer would cost a TCP connection on every call.
    const env = readConsoleEnv({
      ...BASE,
      CONSOLE_DATABASE_URL: "postgres://nobody:nothing@db.invalid:1/console_auth",
    });
    expect(hasConsoleDatabase(env)).toBe(true);
    expect(consoleStorageMode(env)).toBe("durable");
  });

  test("the store the runtime hands out really writes to the database", async () => {
    // The whole point of the file. A selection that returned the right CLASS
    // but a pool over the wrong database would pass every assertion above.
    const env = envWithDatabase();
    await consoleSecretStore(env).put("wired-provider", "wired-client", "wired-secret-value-91af");

    const rows = await pool.query<{ provider_id: string; client_id: string }>(
      "select provider_id, client_id from console_provider_secret",
    );
    expect(rows.rows).toEqual([{ provider_id: "wired-provider", client_id: "wired-client" }]);
  });

  test("resetConsoleRuntime can substitute a store, so tests need no database", () => {
    // The property that keeps the in-memory implementation worth having: a
    // durable store that cannot be substituted would make every test that
    // touches auth need PostgreSQL.
    const substitute = new InMemoryConsoleSecretStore(randomBytes(32));
    resetConsoleRuntime({ store: substitute });
    expect(consoleSecretStore(envWithDatabase())).toBe(substitute);
  });
});
