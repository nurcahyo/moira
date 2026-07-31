// SPIKE — plan 09 wave 4, task T0. NOT SHIPPED CODE, and not a test.
//
// Derives the exact DDL that console migration `0003` must contain if Stage 4B
// takes the session-stamping mechanism, using better-auth's OWN migration
// compiler at the pinned version — the same code path `db/generate.ts` uses.
//
// It is written by hand nowhere: `getMigrations()` introspects a real database
// that already has `0001` and `0002` applied, and reports the diff.
//
//   bun run tests/spike/w4-t0-derive-0003.ts
//
// Uses a THROWAWAY database created and dropped by `withTemporaryDatabase`, so
// it never touches `console_auth_test` (shared with other agents) and never
// touches Moira's `moira` database.

import { getMigrations } from "better-auth/db/migration";

import { applyMigrations, loadMigrations } from "../../db/migrate";
import { scriptPool } from "../../db/dsn";
import { consoleAuthSchemaOptions } from "../../db/schema";
import { withTemporaryDatabase } from "../../db/generate";

/**
 * Duplicated from `w4-t0-spike-auth.ts` rather than imported.
 *
 * That module reaches `lib/auth.ts`, which imports `server-only` — a package
 * whose default export condition is a bare `throw`. The test suite neutralises
 * it with a preload (`bunfig.toml`); a plain `bun run` has no preload, so the
 * import would fail before any SQL was compiled. `db/schema.ts` avoids the same
 * trap the same way, and says so at its `JWKS_PATH` constant.
 */
const SESSION_PROVIDER_FIELD = "providerId";

const BASE_DSN =
  process.env["CONSOLE_TEST_DATABASE_URL"] ??
  "postgres://postgres:postgres@127.0.0.1:5432/console_auth_test";

/** The shipped schema options, plus the one field 4B would add. */
function optionsWithProviderStamp(database: unknown): Parameters<typeof getMigrations>[0] {
  const base = consoleAuthSchemaOptions(database);
  return {
    ...base,
    session: {
      ...base.session,
      additionalFields: {
        [SESSION_PROVIDER_FIELD]: { type: "string", required: false, input: false },
      },
    },
  } as unknown as Parameters<typeof getMigrations>[0];
}

const output = await withTemporaryDatabase(BASE_DSN, async (temporaryDsn) => {
  // 1. Bring the throwaway database up to the CURRENTLY COMMITTED state.
  const migratePool = scriptPool(temporaryDsn);
  try {
    await applyMigrations(migratePool, loadMigrations());
  } finally {
    await migratePool.end();
  }

  // 2. Ask better-auth what is still missing once `additionalFields` is added.
  const diffPool = scriptPool(temporaryDsn);
  try {
    const migrations = await getMigrations(optionsWithProviderStamp(diffPool) as never);
    const pending = migrations.toBeCreated.length + migrations.toBeAdded.length;
    const sql = pending === 0 ? "" : await migrations.compileMigrations();
    return {
      pending,
      toBeCreated: migrations.toBeCreated.map((table) => table.table),
      toBeAdded: migrations.toBeAdded.map((table) => table.table),
      sql: sql.trim(),
    };
  } finally {
    await diffPool.end();
  }
});

console.log(JSON.stringify(output, null, 2));
