// DSN plumbing shared by the `db/` scripts.
//
// Deliberately imports NOTHING from `lib/`. Those modules all start with
// `import "server-only"`, whose non-`react-server` export is a bare `throw` —
// importable under Next.js and under `bun test` (which preloads a shim), but not
// from a plain `bun run` script. Keeping `db/` free of that import is also what
// lets the Helm migration Job run against an image that carries no application
// code at all.
import { Pool } from "pg";

/** Where the migration scripts read the connection string from. */
export const DATABASE_URL_ENV = "CONSOLE_DATABASE_URL";

/**
 * The DSN, from `--url <dsn>` or `$CONSOLE_DATABASE_URL`.
 *
 * Throws rather than defaulting. A migration script that invents a connection
 * string migrates *something* — just not necessarily the database anyone meant.
 */
export function resolveDsn(argv: readonly string[], env: NodeJS.ProcessEnv): string {
  const flag = argv.indexOf("--url");
  const fromFlag = flag === -1 ? undefined : argv[flag + 1];
  const dsn = fromFlag ?? env[DATABASE_URL_ENV];
  if (dsn === undefined || dsn.trim() === "") {
    throw new Error(
      `no database connection string: pass --url <dsn> or set ${DATABASE_URL_ENV}. ` +
        "Use the console's OWN database, never Moira's — they share no tables and " +
        "no migration history.",
    );
  }
  return dsn.trim();
}

/**
 * Split a DSN into "everything but the database name" plus the database name.
 *
 * Used to reach the maintenance database, because `CREATE DATABASE` cannot run
 * from inside the database being created.
 */
export function splitDatabase(dsn: string): { readonly admin: string; readonly database: string } {
  const url = new URL(dsn);
  const database = decodeURIComponent(url.pathname.replace(/^\//, ""));
  if (database === "") {
    throw new Error(`the connection string names no database: ${redactDsn(dsn)}`);
  }
  const admin = new URL(dsn);
  admin.pathname = "/postgres";
  return { admin: admin.toString(), database };
}

/**
 * A DSN with its password replaced.
 *
 * Every error message and log line in `db/` goes through this. A connection
 * string is the one configuration value that carries a credential inline, so a
 * naive `throw new Error(\`cannot connect to ${dsn}\`)` prints the database
 * password into CI output and into `kubectl logs`.
 */
export function redactDsn(dsn: string): string {
  try {
    const url = new URL(dsn);
    if (url.password !== "") url.password = "***";
    return url.toString();
  } catch {
    return "<unparseable connection string>";
  }
}

/** A pool for one short-lived script run. */
export function scriptPool(dsn: string): Pool {
  return new Pool({
    connectionString: dsn,
    max: 1,
    application_name: "moira-console-db",
  });
}
