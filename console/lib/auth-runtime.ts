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
//
// UPDATE (plan 09 Wave 1): finding F15 has been FIXED, and it does NOT satisfy
// that reversal condition — do not read the fix and delete the snapshot.
// Moira now serves `GET /api/v1/admin/setup/sign-in-methods` anonymously, but
// the projection behind it (`PublicSignInMethod`) is deliberately
// `PublicAuthMethod` MINUS `allowed_email_domains` — which is plan 07 decision
// D3, the deny-by-default admin-claim policy, and publishing it anonymously
// would hand any caller the list of domains that can obtain Moira admin — and
// minus `jwks_url`. `resolveAuthConfigs` refuses a row without
// `allowed_email_domains` or `trusted_jwt_issuer_id`, and neither is in the
// anonymous projection. So it is enough to RENDER a sign-in button and not
// enough to RESOLVE the configuration behind it.
//
// The consequence this file owns: the snapshot is per process, so two replicas
// can hold different provider configurations — including different client
// secrets after a rotation — for an unbounded time. That is why
// `charts/moira-console/values.yaml` still pins `replicaCount: 1` even though
// secrets, the JWKS key pair, sessions and rate limits are now all shared
// through the console's database.
import "server-only";

import {
  loadAuthConfigs,
  type AuthConfigProviderProblem,
  type AuthConfigsResolution,
  type ResolvedAuthConfig,
} from "./auth-config";
import { createConsoleAuth, type ConsoleAuth, type ConsoleAuthDatabase } from "./auth";
import { consoleDatabase, hasConsoleDatabase } from "./console-db";
import { InMemoryConsoleSecretStore, type ConsoleSecretStore } from "./console-secrets";
import { PostgresConsoleSecretStore } from "./console-secrets-postgres";
import { consoleEnv, type ConsoleEnv } from "./env";
import { CONSOLE_MESSAGE_KEYS } from "./i18n/keys";
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

/**
 * The snapshot described in the header note.
 *
 * From wave 4B this is the whole successful resolution — N configurations plus
 * the per-provider problems — rather than one config. The problems are carried
 * in the snapshot on purpose: a drifted GitHub row must be reportable on
 * `/login` without taking OIDC sign-in down with it.
 */
let snapshot: SuccessfulResolution | undefined;

type SuccessfulResolution = Extract<AuthConfigsResolution, { ok: true }>;

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

/**
 * What the process can serve right now.
 *
 * ============================================================================
 * WHY THE FAILURE SIDE IS PER PROVIDER (wave 4B)
 * ============================================================================
 *
 * Before 4B this was a single `config` and a single refusal, because a console
 * had one interactive provider and any problem with it was the whole answer.
 * With N providers that shape is actively wrong: a GitHub row whose client
 * secret has drifted out of the console's store would have taken OIDC sign-in
 * down with it, turning one provider's configuration mistake into a total
 * lockout — for a console whose only other way in is the bootstrap system key
 * the operator was told to remove.
 *
 * So `ok: true` carries BOTH: every provider that resolved, and every enabled
 * row that did not with its own keyed reason. `ok: false` is reserved for "no
 * provider resolved at all", which is the only state where there is nothing to
 * render a button from.
 */
export type ConsoleRuntime =
  | {
      readonly ok: true;
      readonly auth: ConsoleAuth;
      readonly configs: readonly ResolvedAuthConfig[];
      /** Enabled rows that did not resolve. Never fatal on its own. */
      readonly problems: readonly AuthConfigProviderProblem[];
    }
  | { readonly ok: false; readonly resolution: Extract<AuthConfigsResolution, { ok: false }> };

/**
 * Refresh the snapshot from Moira using whatever credential `client` carries.
 *
 * Returns the resolution either way, so a caller can surface a configuration
 * problem instead of silently keeping a stale snapshot.
 */
export async function refreshAuthConfig(
  client: MoiraClient,
  env: ConsoleEnv = consoleEnv(),
): Promise<AuthConfigsResolution> {
  const resolution = await loadAuthConfigs(client, consoleSecretStore(env), env.bffIssuerUrl);
  if (resolution.ok) snapshot = resolution;
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
  let resolved = snapshot;

  if (resolved === undefined) {
    if (env.moiraSystemKey === undefined) {
      // Nothing snapshotted and no bootstrap credential: the deadlock in the
      // header note, reported as itself rather than as an opaque 401 from Moira.
      return {
        ok: false,
        resolution: {
          ok: false,
          problem: "no_enabled_provider",
          messageKey: CONSOLE_MESSAGE_KEYS.auth_config_unavailable,
          cacheKey: "unavailable",
        },
      };
    }
    const resolution = await refreshAuthConfig(moiraClientForSetup(env), env);
    if (!resolution.ok) return { ok: false, resolution };
    resolved = resolution;
  }

  const { configs, problems, cacheKey } = resolved;

  if (cachedAuth !== undefined && cachedAuth.cacheKey === cacheKey) {
    return { ok: true, auth: cachedAuth.auth, configs, problems };
  }

  // The pool doubles as Better Auth's `database`: `createKyselyAdapter` detects
  // a `pg.Pool` by its `connect` method and wraps it in Kysely's
  // `PostgresDialect`. One pool serves both the session tables and
  // `console_provider_secret`, so the console holds one connection budget, not
  // two.
  const database = databaseOverride ?? consoleDatabase(env) ?? undefined;
  const auth = createConsoleAuth({
    env,
    configs,
    ...(database === undefined ? {} : { database: database as ConsoleAuthDatabase }),
  });
  // One instance only. A map keyed by digest would keep a rotated client secret
  // alive in memory after it was replaced.
  cachedAuth = { cacheKey, auth };
  return { ok: true, auth, configs, problems };
}
