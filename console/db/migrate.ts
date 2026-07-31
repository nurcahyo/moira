// The console's migration runner.
//
// ============================================================================
// WHY A RUNNER AND NOT `getMigrations().runMigrations()`
// ============================================================================
//
// Better Auth's own `runMigrations()` diffs the live database against the
// current options and applies whatever is missing. That is a fine developer
// convenience and a poor deployment mechanism:
//
//   * it has NO LEDGER, so "has this deployment been migrated?" is answerable
//     only by re-introspecting, and "what changed between two releases?" is not
//     answerable at all;
//   * it applies DDL derived from whatever code the running image happens to
//     contain, so a rollback to an older image silently re-diffs against the
//     older schema;
//   * it is not reviewable — nothing lands in a pull request.
//
// So the SQL is generated once by `db/generate.ts` (from Better Auth's own
// compiler, at the pinned version), committed under `db/migrations/`, reviewed
// like any other change, and applied by this runner against a ledger — the same
// shape as Moira's own `migrations/` directory.
//
// `tests/integration/console-db-migrations.test.ts` closes the loop: after this
// runner finishes, `getMigrations()` must report nothing left to create or add.
// If a better-auth upgrade changes the schema, that test goes red and
// `db/generate.ts` produces the next numbered file.
//
// ============================================================================
// APPEND-ONLY, AND ENFORCED
// ============================================================================
//
// Every applied file's SHA-256 is recorded. Editing a file that has already been
// applied is refused, loudly, naming the file — because the only environments
// where the edit takes effect are the ones that have not run it yet, which makes
// the two halves of a fleet disagree about the schema with nothing to show for
// it.
import { createHash } from "node:crypto";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

import type { Pool, QueryResult, QueryResultRow } from "pg";

import { redactDsn, resolveDsn, scriptPool } from "./dsn";

/**
 * Anything that can run a statement: a `Pool` or a checked-out `PoolClient`.
 *
 * Not cosmetic. `applyMigrations` holds an advisory lock on ONE client and then
 * re-plans; if the re-plan asked the pool for a second connection it would
 * deadlock against its own lock holder whenever the pool's `max` is 1 — which
 * is exactly what `scriptPool` sets, and exactly what a migration Job should
 * set. Threading the client through is the fix.
 */
export interface Queryable {
  query<R extends QueryResultRow = QueryResultRow>(
    text: string,
    values?: unknown[],
  ): Promise<QueryResult<R>>;
}

/** Where the numbered `.sql` files live. */
export const MIGRATIONS_DIR = join(import.meta.dir, "migrations");

/** The ledger. Created by this runner, not by a migration. */
const LEDGER_TABLE = "console_auth_migrations";

/**
 * A cluster-wide lock id for the migration.
 *
 * Two replicas' pre-upgrade hooks, or a hook racing a developer's `bun run
 * db:migrate`, would otherwise both see "0002 is pending" and both run it. The
 * number is arbitrary but must stay fixed forever.
 */
const ADVISORY_LOCK_ID = 0x6d6f6972; // "moir"

export interface Migration {
  readonly name: string;
  readonly sql: string;
  readonly checksum: string;
}

function sha256(text: string): string {
  return createHash("sha256").update(text, "utf8").digest("hex");
}

/**
 * Every migration on disk, in application order.
 *
 * Ordering is by filename, which is why the files are zero-padded and numbered.
 * A non-numeric name would sort unpredictably against them, so it is rejected
 * rather than sorted.
 */
export function loadMigrations(dir: string = MIGRATIONS_DIR): Migration[] {
  const names = readdirSync(dir)
    .filter((name) => name.endsWith(".sql"))
    .sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));

  for (const name of names) {
    if (!/^\d{4}_[a-z0-9_]+\.sql$/.test(name)) {
      throw new Error(
        `migration filename ${name} does not match NNNN_snake_case.sql; ordering is by ` +
          "filename and an unnumbered file has no defined position",
      );
    }
  }

  return names.map((name) => {
    const sql = readFileSync(join(dir, name), "utf8");
    return { name, sql, checksum: sha256(sql) };
  });
}

async function ensureLedger(db: Queryable): Promise<void> {
  await db.query(`
    create table if not exists ${LEDGER_TABLE} (
      name        text primary key,
      checksum    text        not null,
      applied_at  timestamptz not null default now()
    )
  `);
}

interface AppliedRow {
  readonly name: string;
  readonly checksum: string;
}

async function readLedger(db: Queryable): Promise<Map<string, string>> {
  const result = await db.query<AppliedRow>(`select name, checksum from ${LEDGER_TABLE}`);
  return new Map(result.rows.map((row) => [row.name, row.checksum]));
}

export interface MigrationPlan {
  readonly pending: readonly Migration[];
  readonly applied: readonly string[];
}

/**
 * What is left to do, with the append-only rule enforced first.
 *
 * The tamper check runs over ALL applied files rather than only the ones this
 * process would touch: a rewritten `0001` is just as broken when `0002` is the
 * one pending.
 */
export async function planMigrations(
  db: Queryable,
  migrations: readonly Migration[],
): Promise<MigrationPlan> {
  await ensureLedger(db);
  const ledger = await readLedger(db);

  const tampered = migrations
    .filter((m) => ledger.has(m.name) && ledger.get(m.name) !== m.checksum)
    .map((m) => m.name);
  if (tampered.length > 0) {
    throw new Error(
      `these migrations were edited after being applied: ${tampered.join(", ")}. ` +
        "Migrations are append-only — the edit takes effect only in environments that have " +
        "not run it yet, which leaves the fleet disagreeing about the schema. Revert the " +
        "edit and add a new numbered file.",
    );
  }

  const known = new Set(migrations.map((m) => m.name));
  const orphans = [...ledger.keys()].filter((name) => !known.has(name));
  if (orphans.length > 0) {
    throw new Error(
      `the database has applied migrations this build does not contain: ${orphans.join(", ")}. ` +
        "This is an older image pointed at a newer database; roll the image forward rather " +
        "than migrating backwards.",
    );
  }

  return {
    pending: migrations.filter((m) => !ledger.has(m.name)),
    applied: [...ledger.keys()].sort(),
  };
}

/**
 * Apply every pending migration.
 *
 * One transaction per file, and the ledger row is written inside that same
 * transaction — so a file that fails halfway leaves neither its DDL nor its
 * ledger entry behind. (PostgreSQL has transactional DDL; this would not be
 * safe on MySQL.)
 */
export async function applyMigrations(
  pool: Pool,
  migrations: readonly Migration[],
  log: (message: string) => void = () => {},
): Promise<readonly string[]> {
  const client = await pool.connect();
  try {
    await client.query("select pg_advisory_lock($1)", [ADVISORY_LOCK_ID]);
    // Re-planned while holding the lock: another runner may have applied
    // everything between our first look and our turn.
    const plan = await planMigrations(client, migrations);
    const done: string[] = [];
    for (const migration of plan.pending) {
      await client.query("begin");
      try {
        await client.query(migration.sql);
        await client.query(`insert into ${LEDGER_TABLE} (name, checksum) values ($1, $2)`, [
          migration.name,
          migration.checksum,
        ]);
        await client.query("commit");
      } catch (error) {
        await client.query("rollback");
        throw new Error(`migration ${migration.name} failed: ${(error as Error).message}`);
      }
      log(`applied ${migration.name}`);
      done.push(migration.name);
    }
    return done;
  } finally {
    await client.query("select pg_advisory_unlock($1)", [ADVISORY_LOCK_ID]).catch(() => {});
    client.release();
  }
}

/* -------------------------------------------------------------------------- */
/* CLI                                                                        */
/* -------------------------------------------------------------------------- */

/**
 * `--check` reports pending migrations and exits 1 without touching the
 * database. It is what a readiness gate or a CI job should run: "is this
 * database at the schema this image expects?".
 */
async function main(): Promise<number> {
  const argv = process.argv.slice(2);
  const check = argv.includes("--check");
  const dsn = resolveDsn(argv, process.env);
  const migrations = loadMigrations();

  const pool = scriptPool(dsn);
  try {
    if (check) {
      // The pool is safe here: `--check` takes no advisory lock, so nothing is
      // holding the single connection `scriptPool` allows.
      const plan = await planMigrations(pool, migrations);
      if (plan.pending.length === 0) {
        console.log(
          `${redactDsn(dsn)}: up to date (${plan.applied.length} migration(s) applied)`,
        );
        return 0;
      }
      console.error(
        `${redactDsn(dsn)}: ${plan.pending.length} pending migration(s): ` +
          plan.pending.map((m) => m.name).join(", "),
      );
      return 1;
    }

    const done = await applyMigrations(pool, migrations, (message) => console.log(message));
    console.log(
      done.length === 0
        ? `${redactDsn(dsn)}: already up to date`
        : `${redactDsn(dsn)}: applied ${done.length} migration(s)`,
    );
    return 0;
  } finally {
    await pool.end();
  }
}

if (import.meta.main) {
  try {
    process.exitCode = await main();
  } catch (error) {
    console.error(`console migration failed: ${(error as Error).message}`);
    process.exitCode = 1;
  }
}
