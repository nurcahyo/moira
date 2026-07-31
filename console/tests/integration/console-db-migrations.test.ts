// The committed migrations, against a real PostgreSQL.
//
// ============================================================================
// THE DRIFT GATE
// ============================================================================
//
// `db/migrations/0001_better_auth_core.sql` was produced by better-auth's own
// compiler at the pinned version. Nothing keeps it correct as the dependency
// moves except this file: after every committed migration is applied,
// `getMigrations()` must report NOTHING left to create and nothing left to add.
// Bump `better-auth` in a way that touches the schema and this goes red, naming
// the tables.
//
// The second half of the gate is that the schema is generated from options that
// match the RUNNING instance. `db/schema.ts` cannot import `createConsoleAuth`
// (that needs a resolved provider configuration, which a migration job does not
// have), so the two are compared here directly: both are handed the same empty
// database and their table/field sets must be identical.
import { afterAll, beforeAll, expect, test } from "bun:test";
import { randomBytes } from "node:crypto";

import { getMigrations } from "better-auth/db/migration";
import { Pool } from "pg";

import { scriptPool } from "../../db/dsn";
import { withTemporaryDatabase } from "../../db/generate";
import { applyMigrations, loadMigrations, planMigrations } from "../../db/migrate";
import { consoleAuthSchemaOptions } from "../../db/schema";
import { createConsoleAuth } from "../../lib/auth";
import { readConsoleEnv } from "../../lib/env";
import {
  DATABASE_TESTS_SKIPPED,
  describeDatabase,
  openConsoleTestDatabase,
  testDatabaseUrl,
} from "../support/console-db";

let pool: Pool;

beforeAll(async () => {
  if (DATABASE_TESTS_SKIPPED) return;
  pool = await openConsoleTestDatabase();
});

afterAll(async () => {
  await pool?.end();
});

describeDatabase("the migration runner", () => {
  test("every committed migration is numbered and ordered", () => {
    const migrations = loadMigrations();
    expect(migrations.length).toBeGreaterThanOrEqual(2);
    expect(migrations.map((m) => m.name)).toEqual([...migrations.map((m) => m.name)].sort());
    expect(migrations[0]?.name).toBe("0001_better_auth_core.sql");
  });

  test("re-running is a no-op", async () => {
    // `openConsoleTestDatabase` already applied everything. The ledger is what
    // makes this cheap and, more importantly, what makes it SAFE: without one,
    // a second run would re-execute `create table` and fail.
    const applied = await applyMigrations(pool, loadMigrations());
    expect(applied).toEqual([]);

    const plan = await planMigrations(pool, loadMigrations());
    expect(plan.pending).toEqual([]);
    expect(plan.applied).toContain("0001_better_auth_core.sql");
    expect(plan.applied).toContain("0002_console_provider_secret.sql");
  });

  test("editing an applied migration is refused", async () => {
    // Append-only, enforced. The alternative is a fleet where half the pods ran
    // one version of `0001` and half ran another, with nothing recording it.
    const tampered = loadMigrations().map((m) =>
      m.name === "0001_better_auth_core.sql"
        ? { ...m, checksum: "0000000000000000000000000000000000000000000000000000000000000000" }
        : m,
    );
    await expect(planMigrations(pool, tampered)).rejects.toThrow(/edited after being applied/);
  });

  test("a database ahead of this build is refused, not migrated backwards", async () => {
    const truncated = loadMigrations().filter((m) => m.name !== "0002_console_provider_secret.sql");
    await expect(planMigrations(pool, truncated)).rejects.toThrow(/this build does not contain/);
  });

  test("runs against a single-connection pool without deadlocking on its own lock", async () => {
    // REGRESSION. `applyMigrations` takes `pg_advisory_lock` on one checked-out
    // client and then re-plans while holding it. The first version re-planned
    // through the POOL, so with `max: 1` — which is what `scriptPool` uses, and
    // what a migration Job should use — it waited forever for a connection its
    // own lock holder was occupying. It did not fail; it hung, which is the
    // shape that survives a CI timeout as "flaky".
    const single = scriptPool(testDatabaseUrl());
    try {
      const applied = await Promise.race([
        applyMigrations(single, loadMigrations()),
        new Promise<never>((_, reject) =>
          setTimeout(() => reject(new Error("applyMigrations deadlocked on a max:1 pool")), 15_000),
        ),
      ]);
      expect(applied).toEqual([]);
    } finally {
      await single.end();
    }
  }, 30_000);
});

describeDatabase("the schema matches what better-auth wants", () => {
  test("nothing is left to create or add after migrating", async () => {
    // NOTE: better-auth logs `Field lastRequest in table rateLimit has a
    // different type in the database. Expected number but got int8` during this
    // call. That is an upstream inconsistency — its own compiler emits `bigint`
    // for that field and its own `matchType` does not recognise `int8` as a
    // `number`. It is a WARNING and does not appear in `toBeAdded`, so the
    // assertions below are unaffected. Do not "fix" it by editing the generated
    // SQL: the next `db:generate` would emit `bigint` again.
    const migrations = await getMigrations(consoleAuthSchemaOptions(pool) as never);
    expect(
      migrations.toBeCreated.map((t) => t.table),
      "better-auth wants tables the committed migrations do not create. Regenerate with " +
        "`bun run db:generate` and commit the output as the NEXT numbered file.",
    ).toEqual([]);
    expect(
      migrations.toBeAdded.map((t) => ({ table: t.table, fields: Object.keys(t.fields) })),
      "better-auth wants columns the committed migrations do not create. Regenerate with " +
        "`bun run db:generate` and commit the output as the NEXT numbered file.",
    ).toEqual([]);
  });

  test("the console's own table is present and is not better-auth's business", async () => {
    const result = await pool.query<{ column_name: string }>(
      `select column_name from information_schema.columns
        where table_name = 'console_provider_secret' order by column_name`,
    );
    expect(result.rows.map((r) => r.column_name)).toEqual([
      "ciphertext",
      "client_id",
      "envelope_version",
      "iv",
      "provider_id",
      "updated_at",
    ]);
  });

  test("db/schema.ts and lib/auth.ts describe the same schema", async () => {
    // The one way the generator can rot: someone adds a plugin to the running
    // instance and not to the generator, the migration stops covering a table,
    // and the first sign-in after deploy fails on a missing relation.
    //
    // Both option sets are diffed against the SAME empty database, so the
    // comparison is of what each WANTS rather than of what either found.
    const env = readConsoleEnv({
      NODE_ENV: "test",
      MOIRA_API_URL: "https://moira.invalid",
      CONSOLE_PUBLIC_ORIGIN: "https://console.invalid",
      MOIRA_ADMIN_API_AUDIENCE: "moira-admin",
      BETTER_AUTH_SECRET: "a-secret-that-is-at-least-32-characters",
      CONSOLE_SECRET_ENCRYPTION_KEY: randomBytes(32).toString("base64"),
    });

    await withTemporaryDatabase(testDatabaseUrl(), async (temporaryDsn) => {
      const describe_ = async (options: unknown) => {
        const scratch = new Pool({ connectionString: temporaryDsn, max: 1 });
        try {
          const m = await getMigrations(
            { ...(options as Record<string, unknown>), database: scratch } as never,
          );
          return m.toBeCreated
            .map((t) => `${t.table}: ${Object.keys(t.fields).sort().join(",")}`)
            .sort();
        } finally {
          await scratch.end();
        }
      };

      const generatorTables = await describe_(consoleAuthSchemaOptions(null));

      const runtimePool = new Pool({ connectionString: temporaryDsn, max: 1 });
      const auth = createConsoleAuth({
        env,
        config: {
          providerId: "moira-console-idp",
          method: "generic_oidc",
          moiraProviderId: "p",
          moiraProviderVersion: 1,
          issuer: "https://idp.invalid",
          discoveryUrl: "https://idp.invalid/.well-known/openid-configuration",
          authorizationUrl: null,
          tokenUrl: null,
          userInfoUrl: null,
          clientId: "client",
          clientSecret: "secret",
          scopes: ["openid", "email"],
          allowedEmailDomains: ["example.com"],
          trustedJwtIssuerId: "00000000-0000-0000-0000-000000000000",
          cacheKey: "k",
        },
        database: runtimePool,
      });
      const runtimeTables = await describe_(auth.options);
      await runtimePool.end();

      expect(
        generatorTables,
        "db/schema.ts no longer describes the same tables as lib/auth.ts. A plugin was " +
          "added to one and not the other; the committed migrations now under-cover the " +
          "running instance.",
      ).toEqual(runtimeTables);
      expect(generatorTables.length).toBeGreaterThan(0);
    });
  }, 60_000);
});
