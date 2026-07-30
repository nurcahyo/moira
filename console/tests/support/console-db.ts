// A real PostgreSQL database for the integration suite.
//
// ============================================================================
// NO SILENT SKIP
// ============================================================================
//
// The obvious harness — "if $DATABASE_URL is unset, skip" — is how this
// repository has previously turned a gate green without running it: a wrong or
// missing URL made the whole database suite vanish and the run still reported
// success. So this harness has exactly one escape hatch, `CONSOLE_SKIP_DB_TESTS=1`,
// it must be set deliberately, and `console-db-availability.test.ts` FAILS when
// it is set — a skipped database suite is a red run, not a quiet one.
//
// Everything else is an error: an unreachable server, a bad password and a
// missing database each throw with the DSN redacted and the remedy named.
//
// ============================================================================
// WHICH DATABASE
// ============================================================================
//
// `CONSOLE_TEST_DATABASE_URL`, defaulting to `console_auth_test` on the same
// local server Moira's own tests use. It is a SEPARATE DATABASE from Moira's on
// purpose — `MOIRA_TEST_DATABASE_URL` points at `moira`, whose schema is owned
// by the repository-root `migrations/` and applied by Moira's binary. Two
// independent migration ledgers in one database is the failure this default
// exists to make impossible to reach by accident.
//
// The database is CREATED if it does not exist, and each test file gets its own
// freshly-migrated schema-less copy by truncating rather than by dropping — see
// `resetConsoleTestDatabase`.
import { describe } from "bun:test";
import { Pool } from "pg";

import { applyMigrations, loadMigrations } from "../../db/migrate";
import { redactDsn, splitDatabase } from "../../db/dsn";

/** Same server as Moira's test database, different database. */
export const DEFAULT_TEST_DSN = "postgres://postgres:postgres@127.0.0.1:5432/console_auth_test";

export const SKIP_ENV = "CONSOLE_SKIP_DB_TESTS";

/** Is the database suite deliberately disabled for this run? */
export function databaseTestsSkipped(env: NodeJS.ProcessEnv = process.env): boolean {
  return env[SKIP_ENV] === "1" || env[SKIP_ENV] === "true";
}

/**
 * `describe` for a database-backed group, honouring the one escape hatch.
 *
 * Evaluated once at import. Every hook in a file that uses this must ALSO check
 * `DATABASE_TESTS_SKIPPED` — Bun runs top-level `beforeAll`/`afterEach` even
 * when every `describe` in the file is skipped, so an unguarded hook would still
 * try to connect and turn a deliberate skip into a failure.
 */
export const DATABASE_TESTS_SKIPPED = databaseTestsSkipped();

/** @see DATABASE_TESTS_SKIPPED */
export const describeDatabase = describe.skipIf(DATABASE_TESTS_SKIPPED);

/** The DSN under test. */
export function testDatabaseUrl(env: NodeJS.ProcessEnv = process.env): string {
  return env["CONSOLE_TEST_DATABASE_URL"] ?? DEFAULT_TEST_DSN;
}

/**
 * Create the test database if it is not there yet.
 *
 * `42P01`/`3D000` (`database ... does not exist`) is the one connection error
 * worth recovering from automatically: it is what a fresh checkout hits, and
 * making a developer run one `createdb` before any test passes is how a suite
 * becomes "the one nobody runs locally".
 */
async function ensureDatabase(dsn: string): Promise<void> {
  const { admin, database } = splitDatabase(dsn);
  const adminPool = new Pool({ connectionString: admin, max: 1, connectionTimeoutMillis: 5_000 });
  try {
    await adminPool.query(`create database "${database}"`);
  } catch (error) {
    // 42P04 = duplicate_database. Anything else is a real problem.
    if ((error as { code?: string }).code !== "42P04") throw error;
  } finally {
    await adminPool.end();
  }
}

/**
 * A migrated database, and a pool over it.
 *
 * Callers must `await pool.end()`. Bun runs test files in one process per file,
 * so a leaked pool keeps the run alive after the last assertion.
 */
export async function openConsoleTestDatabase(dsn: string = testDatabaseUrl()): Promise<Pool> {
  try {
    await ensureDatabase(dsn);
  } catch (error) {
    throw new Error(
      `cannot reach PostgreSQL at ${redactDsn(dsn)}: ${(error as Error).message}\n` +
        "The console's integration suite needs a real database — an in-memory store passes " +
        "'write then read' trivially and proves nothing about durability.\n" +
        "Start PostgreSQL, or point CONSOLE_TEST_DATABASE_URL at one. Set " +
        `${SKIP_ENV}=1 to disable the suite deliberately (which reds ` +
        "console-db-availability.test.ts, on purpose).",
    );
  }

  const pool = new Pool({ connectionString: dsn, max: 4, connectionTimeoutMillis: 5_000 });
  pool.on("error", () => {});
  await applyMigrations(pool, loadMigrations());
  return pool;
}

/**
 * Empty every table this suite writes to, without dropping the schema.
 *
 * `truncate ... cascade` rather than `drop database`: the migration ledger stays
 * intact, so a second test file reuses the migrated schema instead of paying for
 * it again, and the ledger itself is left alone so an accidental double-apply
 * would still be caught.
 */
export async function resetConsoleTestDatabase(pool: Pool): Promise<void> {
  await pool.query(
    'truncate table console_provider_secret, "jwks", "rateLimit", "session", "account", ' +
      '"verification", "user" cascade',
  );
}
