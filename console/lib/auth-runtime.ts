// @server-only
//
// Process-level wiring: environment + secret store + Moira client → a Better
// Auth instance for the current configuration.
//
// ============================================================================
// THE BOOTSTRAP DEADLOCK, AND HOW THIS RESOLVES IT
// ============================================================================
//
// §7.2 says the console reads its auth configuration at runtime from Moira's
// DB-backed settings rather than from build-time env. Taken literally that is
// CIRCULAR for the sign-in path, and neither plan 08's body nor its §0 records
// it:
//
//   * every read of that configuration needs a credential —
//     `GET /api/v1/admin/auth/providers` declares
//     `[bearerAuth, systemKeyAuth, consumerKeyAuth]`, and even the narrowed
//     bootstrap projection `GET /api/v1/admin/setup/auth-methods` declares
//     `[bearerAuth, systemKeyAuth]` (verified against `docs/openapi.json`;
//     `GET /api/v1/admin/setup/claim-status` is the ONLY anonymous operation on
//     the whole admin surface);
//   * the console's `bearerAuth` is the JWT it mints for a signed-in operator;
//   * to sign that operator in it needs the auth configuration.
//
// So a console whose operator has removed `MOIRA_SYSTEM_KEY` after setup — the
// thing an operator is expected to do with a bootstrap credential — can never
// again offer a sign-in button.
//
// RESOLUTION (decision, with its reversal condition):
//
//   1. While the system key is present, read the configuration live from Moira
//      and snapshot it.
//   2. Once it is gone, serve sign-in from the snapshot, and refresh the
//      snapshot on every request that DOES carry an operator credential.
//
// The cost is bounded and nameable: a provider changed in Moira while nobody is
// signed in keeps serving from the snapshot until somebody signs in with the
// old one. Since the old configuration is what they would have to sign in with
// anyway, that is the only self-consistent behaviour available.
//
// Rejected alternative: keep `MOIRA_SYSTEM_KEY` set permanently. It works, and
// it means the console permanently holds a credential that bypasses
// `admin_identities` entirely — every admin call could be made as the bootstrap
// key, and the audit trail would stop naming humans. `moiraClientForSession`
// deliberately does not pass the system key for exactly this reason; making the
// deployment hold it forever would undo that.
//
// REVERSAL CONDITION: if Moira ever makes `GET /api/v1/admin/setup/auth-methods`
// anonymous — which its narrowed `PublicAuthMethod` projection suggests was the
// intent, since every field in it is already safe to show an unauthenticated
// visitor — delete the snapshot and read it live. That is the better design;
// this is the one available without modifying Moira, which plan 08 does not do.
import "server-only";

import {
  loadAuthConfig,
  type AuthConfigResolution,
  type ResolvedAuthConfig,
} from "./auth-config";
import { createConsoleAuth, type ConsoleAuth, type ConsoleAuthDatabase } from "./auth";
import { consoleDatabase, hasConsoleDatabase } from "./console-db";
import { InMemoryConsoleSecretStore, type ConsoleSecretStore } from "./console-secrets";
import { PostgresConsoleSecretStore } from "./console-secrets-postgres";
import { consoleEnv, type ConsoleEnv } from "./env";
import { MoiraClient } from "./moira-client";
import { moiraClientForSetup } from "./moira-session";

/* -------------------------------------------------------------------------- */
/* Storage selection                                                          */
/* -------------------------------------------------------------------------- */

/**
 * Which storage the process is running on.
 *
 * Exported because it is the single thing an operator most needs to be able to
 * ask, and because a health endpoint reporting "durable" is a much better
 * signal than discovering after a restart that it was not.
 */
export type ConsoleStorageMode = "durable" | "ephemeral";

/**
 * The decision, in one place.
 *
 * `lib/env.ts` has already refused to boot a production process without a
 * configured database, so `"ephemeral"` here is only ever a development or test
 * process. Both halves — the Better Auth adapter and the secret store — are
 * chosen from the SAME answer: a process with a durable session store but an
 * in-memory secret store, or the reverse, is a configuration nobody asked for
 * and every failure mode of both.
 */
export function consoleStorageMode(env: ConsoleEnv = consoleEnv()): ConsoleStorageMode {
  return hasConsoleDatabase(env) ? "durable" : "ephemeral";
}

/* -------------------------------------------------------------------------- */
/* Process singletons                                                         */
/* -------------------------------------------------------------------------- */

let secretStore: ConsoleSecretStore | undefined;

/** The console's OAuth client-secret store (D7). */
export function consoleSecretStore(env: ConsoleEnv = consoleEnv()): ConsoleSecretStore {
  if (secretStore === undefined) {
    const pool = consoleDatabase(env);
    secretStore =
      pool === null
        ? new InMemoryConsoleSecretStore(env.secretEncryptionKey)
        : new PostgresConsoleSecretStore(pool, env.secretEncryptionKey);
  }
  return secretStore;
}

/** The snapshot described in the header note. */
let snapshot: ResolvedAuthConfig | undefined;

/** The memoised instance, keyed by the configuration digest. */
let cachedAuth: { readonly cacheKey: string; readonly auth: ConsoleAuth } | undefined;

/**
 * Injected by tests. `undefined` means "decide from the environment" — the
 * console's own pool when `CONSOLE_DATABASE_URL` is set, Better Auth's
 * in-memory adapter otherwise.
 */
let databaseOverride: ConsoleAuthDatabase | undefined;

export interface ConsoleRuntimeOverrides {
  readonly store?: ConsoleSecretStore;
  readonly database?: ConsoleAuthDatabase;
}

/** Test seam. Also the operator-facing "reload configuration" path. */
export function resetConsoleRuntime(overrides: ConsoleRuntimeOverrides = {}): void {
  secretStore = overrides.store;
  databaseOverride = overrides.database;
  snapshot = undefined;
  cachedAuth = undefined;
}

/* -------------------------------------------------------------------------- */
/* Resolution                                                                 */
/* -------------------------------------------------------------------------- */

export type ConsoleRuntime =
  | { readonly ok: true; readonly auth: ConsoleAuth; readonly config: ResolvedAuthConfig }
  | { readonly ok: false; readonly resolution: Extract<AuthConfigResolution, { ok: false }> };

/**
 * Refresh the snapshot from Moira using whatever credential `client` carries.
 *
 * Returns the resolution either way, so a caller can surface a configuration
 * problem instead of silently keeping a stale snapshot.
 */
export async function refreshAuthConfig(
  client: MoiraClient,
  env: ConsoleEnv = consoleEnv(),
): Promise<AuthConfigResolution> {
  const resolution = await loadAuthConfig(client, consoleSecretStore(env));
  if (resolution.ok) snapshot = resolution.config;
  return resolution;
}

/**
 * The Better Auth instance for the current configuration.
 *
 * Never throws for a *configuration* problem — those come back as
 * `{ ok: false }` with a message key, because "no provider is enabled yet" is
 * the normal first-run state and not an error condition.
 */
export async function consoleRuntime(env: ConsoleEnv = consoleEnv()): Promise<ConsoleRuntime> {
  let config = snapshot;

  if (config === undefined) {
    if (env.moiraSystemKey === undefined) {
      // Nothing snapshotted and no bootstrap credential: the deadlock in the
      // header note, reported as itself rather than as an opaque 401 from Moira.
      return {
        ok: false,
        resolution: {
          ok: false,
          problem: "no_enabled_provider",
          messageKey: "console.error.auth_config_unavailable",
        },
      };
    }
    const resolution = await refreshAuthConfig(moiraClientForSetup(env), env);
    if (!resolution.ok) return { ok: false, resolution };
    config = resolution.config;
  }

  if (cachedAuth !== undefined && cachedAuth.cacheKey === config.cacheKey) {
    return { ok: true, auth: cachedAuth.auth, config };
  }

  // The pool doubles as Better Auth's `database`: `createKyselyAdapter` detects
  // a `pg.Pool` by its `connect` method and wraps it in Kysely's
  // `PostgresDialect`. One pool serves both the session tables and
  // `console_provider_secret`, so the console holds one connection budget, not
  // two.
  const database = databaseOverride ?? consoleDatabase(env) ?? undefined;
  const auth = createConsoleAuth({
    env,
    config,
    ...(database === undefined ? {} : { database: database as ConsoleAuthDatabase }),
  });
  // One instance only. A map keyed by digest would keep a rotated client secret
  // alive in memory after it was replaced.
  cachedAuth = { cacheKey: config.cacheKey, auth };
  return { ok: true, auth, config };
}
