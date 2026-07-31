// @server-only
//
// The connection pool for the console's OWN PostgreSQL database.
//
// ============================================================================
// WHAT LIVES IN THIS DATABASE, AND WHY IT HAS TO BE DURABLE
// ============================================================================
//
//   user, session, account, verification   Better Auth core
//   jwks                                   the jwt plugin's ES256 key pair
//   console_provider_secret                the OAuth client secret (D7)
//   console_auth_migrations                the migration ledger
//
// Plan 08 ran all of that in process memory and documented the three costs
// (`lib/auth.ts`'s DELIBERATE SCOPE LIMIT, `lib/console-secrets.ts`'s, and
// `charts/moira-console/values.yaml`'s `replicaCount: 1`). The sharpest of the
// three is the key pair: with `memoryAdapter` it is regenerated on every process
// start, so — quoting `lib/auth.ts` — "the JWKS document Moira fetched a minute
// ago stops verifying newly minted tokens". A restart is therefore an outage of
// unbounded length, ending only when Moira's JWKS cache happens to expire.
//
// ============================================================================
// THIS IS NOT MOIRA'S DATABASE
// ============================================================================
//
// `CONSOLE_DATABASE_URL` must name a database of the console's own. The two
// share no tables, no migration history and no ownership: Moira's schema is
// managed by `migrations/` at the repository root and applied by Moira's own
// binary, and pointing both at one database would leave two independent
// migration ledgers writing into one `search_path`. They may share a *server*;
// they may not share a *database*.
//
// It is also the reason `charts/moira-console/values.yaml` carries its own
// `networkPolicy.database` egress rule rather than reusing Moira's.
import "server-only";

import { Pool } from "pg";

import { consoleEnv, type ConsoleEnv } from "./env";

/**
 * Pool sizing.
 *
 * Small on purpose. Every query this console makes is a single-row read or
 * write on the sign-in path; the pool exists to avoid a TCP+TLS handshake per
 * request, not to run concurrent analytics. A large pool multiplied by
 * `replicaCount` is how a console exhausts `max_connections` on a database
 * that Moira also needs.
 */
const POOL_MAX = 8;

/** Fail a checkout rather than queue forever behind a dead database. */
const CONNECTION_TIMEOUT_MS = 5_000;

/** Return an idle connection to the server rather than holding it open. */
const IDLE_TIMEOUT_MS = 30_000;

let pool: Pool | undefined;

/**
 * The pool, or `null` when no `CONSOLE_DATABASE_URL` is configured.
 *
 * `null` is reachable ONLY outside production — `lib/env.ts` makes the variable
 * a hard boot failure under `NODE_ENV=production`, so a deployment cannot end up
 * on the ephemeral path by omission. In development and under test the ephemeral
 * path is the default, which is what keeps `bun test` runnable without a
 * database.
 */
export function consoleDatabase(env: ConsoleEnv = consoleEnv()): Pool | null {
  if (env.consoleDatabaseUrl === undefined) return null;
  pool ??= createConsolePool(env.consoleDatabaseUrl);
  return pool;
}

/**
 * Is this process configured for durable storage?
 *
 * Answers the question WITHOUT constructing a pool, which is why it exists
 * rather than callers writing `consoleDatabase(env) !== null`: asking "am I
 * durable?" should not open sockets. It is also what keeps the connection
 * string readable in exactly two modules — `lib/env.ts` validates it, this one
 * consumes it, and `tests/unit/architecture/server-only-guards.test.ts` enforces
 * that there is no third.
 */
export function hasConsoleDatabase(env: ConsoleEnv = consoleEnv()): boolean {
  return env.consoleDatabaseUrl !== undefined;
}

/**
 * A pool over an explicit DSN.
 *
 * Exported so tests can hold a pool of their own — and so a test can create a
 * SECOND, independent pool over the same database, which is how durability is
 * demonstrated without spawning a process.
 */
export function createConsolePool(connectionString: string): Pool {
  const created = new Pool({
    connectionString,
    max: POOL_MAX,
    connectionTimeoutMillis: CONNECTION_TIMEOUT_MS,
    idleTimeoutMillis: IDLE_TIMEOUT_MS,
    application_name: "moira-console",
  });
  // `pg` emits `error` on idle clients when the server closes a connection
  // (a failover, a `pg_terminate_backend`, an idle timeout on the server side).
  // An unhandled `error` event on an EventEmitter is a process-level crash in
  // Node, so a routine database failover would take the console down. The pool
  // discards the client and hands out a fresh one by itself; there is nothing to
  // do here but refuse to die.
  created.on("error", () => {});
  return created;
}

/** Test seam, and the shutdown path. Safe to call when no pool was created. */
export async function closeConsoleDatabase(): Promise<void> {
  const existing = pool;
  pool = undefined;
  if (existing !== undefined) await existing.end();
}
