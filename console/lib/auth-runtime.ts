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
//
// ============================================================================
// HOW THE SNAPSHOT STOPS BEING STALE (issue #152)
// ============================================================================
//
// "For an unbounded time" above was literal, and it was the defect. Until #152
// the snapshot was written once and never again: `refreshAuthConfig` had exactly
// one caller — `consoleRuntime` — and it ran only when no snapshot existed yet.
// An operator who re-pointed the provider through `/setup` or through Moira's
// admin API kept being served the OLD configuration until the process was
// restarted, and nothing said so: sign-in failed with `ECONNREFUSED` against an
// endpoint that no longer existed, which reads as "the IdP is down".
//
// The repair mirrors Moira's own runtime-config invalidation
// (`docs/runtime-cache-invalidation.md`) rather than inventing a second
// mechanism, because that document already settles the shape of this problem:
//
//   EXPLICIT INVALIDATION is the primary mechanism. In Moira that is the
//   `moira_runtime_config` NOTIFY trigger; in the console it is
//   `invalidateAuthConfig`, called by the one writer the console owns — the
//   setup wizard's provisioning route. A provider re-pointed through the wizard
//   is therefore in effect on the very NEXT request, with no TTL to wait out.
//
//   A BOUNDED TTL is the backstop, for every change the console cannot observe:
//   a write through Moira's admin API, a write by another replica, a write by
//   `moirad` itself. Moira states the same rule for the same reason —
//   "notifications are not a durable event bus … runtime caches retain TTLs so a
//   missed notification cannot create permanent staleness".
//
// REJECTED: dropping the cache and resolving per request. `loadAuthConfigs`
// makes two Moira calls plus one secret-store read per enabled provider, and
// `consoleRuntime()` is on the hot path of EVERY authenticated request
// (`withConsoleSession`), every authenticated page render, and every
// `/api/auth/*` call including `get-session`. That is a network round trip per
// request to Moira on the path whose failure mode is "the console is down".
//
// The TTL is shorter than Moira's own 300s (`provider_settings_cache_ttl_seconds`)
// deliberately: there it is a pure safety net behind a NOTIFY listener that sees
// every write, whereas here it is the ONLY thing that sees an out-of-band write,
// so it is the staleness bound an operator actually experiences.
//
// ============================================================================
// AND WHEN IT CANNOT BE REFRESHED, IT SAYS SO
// ============================================================================
//
// The bootstrap deadlock above has a second half: a process with no
// `MOIRA_SYSTEM_KEY` cannot re-read the configuration at all, so its snapshot
// cannot be refreshed however stale it gets. Serving it anyway is still the only
// self-consistent behaviour — the old configuration is what an operator would
// have to sign in with — but it is served with `stale: true`, which `/login`
// renders as an operator-visible notice. Silence was as much the defect as the
// staleness: #152's reproduction was an operator staring at a fetch error with
// nothing anywhere naming configuration as the cause.
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
import { isMoiraRequestError } from "./errors";
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

/* -------------------------------------------------------------------------- */
/* Staleness bounds                                                           */
/* -------------------------------------------------------------------------- */

/**
 * How long a resolved snapshot may be served before it is re-read from Moira.
 *
 * The backstop, not the mechanism — `invalidateAuthConfig` is what makes a
 * wizard write take effect immediately. This bounds the changes the console
 * cannot see: a write through Moira's admin API, or by another replica.
 *
 * 60s rather than Moira's 300s (`provider_settings_cache_ttl_seconds`) because
 * there the TTL sits behind a NOTIFY listener that observes every write and here
 * nothing does, so this is the staleness an operator actually experiences. It is
 * also, at one `loadAuthConfigs` per minute per process, nowhere near a per-request
 * cost: `consoleRuntime()` runs on every authenticated request and page render.
 */
export const AUTH_CONFIG_SNAPSHOT_TTL_MS = 60_000;

/**
 * How long after a FAILED refresh before another is attempted.
 *
 * Without it, a Moira outage turns every request arriving at an expired snapshot
 * into another doomed round trip — the console hammering a backend that is
 * already down, on the path it needs most to stay cheap.
 */
export const AUTH_CONFIG_REFRESH_RETRY_MS = 10_000;

/**
 * The snapshot described in the header note, plus when it was read.
 *
 * From wave 4B `resolution` is the whole successful resolution — N configurations
 * plus the per-provider problems — rather than one config. The problems are
 * carried in the snapshot on purpose: a drifted GitHub row must be reportable on
 * `/login` without taking OIDC sign-in down with it.
 */
interface AuthConfigSnapshot {
  readonly resolution: SuccessfulResolution;
  /** When this configuration was last read from Moira. Drives the TTL. */
  readonly resolvedAtMs: number;
  /**
   * When a refresh was last ATTEMPTED, successful or not. Drives the retry
   * floor, and is therefore not the same clock as `resolvedAtMs`: a snapshot
   * whose refresh keeps failing stays old while its attempts stay recent.
   */
  readonly attemptedAtMs: number;
}

let snapshot: AuthConfigSnapshot | undefined;

/**
 * The clock, injectable through `resetConsoleRuntime`.
 *
 * A TTL test that cannot move time either sleeps for the real interval or
 * asserts nothing. Neither is a test of the property.
 */
let clock: () => number = () => Date.now();

type SuccessfulResolution = Extract<AuthConfigsResolution, { ok: true }>;
type FailedResolution = Extract<AuthConfigsResolution, { ok: false }>;

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
  /** Substitute clock, so a TTL test can move time instead of sleeping. */
  readonly now?: () => number;
}

/** Test seam. Also the operator-facing "reload everything" path. */
export function resetConsoleRuntime(overrides: ConsoleRuntimeOverrides = {}): void {
  secretStore = overrides.store;
  databaseOverride = overrides.database;
  clock = overrides.now ?? (() => Date.now());
  snapshot = undefined;
  cachedAuth = undefined;
}

/**
 * Drop the snapshot so the next `consoleRuntime()` re-reads it from Moira.
 *
 * ============================================================================
 * THE CONSOLE'S HALF OF `docs/runtime-cache-invalidation.md` (issue #152)
 * ============================================================================
 *
 * Moira invalidates its own auth-provider-settings cache from a database trigger,
 * because every writer goes through its database. The console has no such
 * vantage point — it is a client of Moira's HTTP API — so it invalidates at the
 * one place it *is* the writer: `app/api/setup/route.ts`, immediately after a
 * provisioning run commits. That is the case #152 was actually met in, and the
 * one where a TTL's worth of staleness is least excusable, because the operator
 * is standing at the wizard watching it happen.
 *
 * Deliberately NOT `resetConsoleRuntime()`. That also drops the secret store and
 * the injected database, which a shipped call site must never do: the store holds
 * the pool, and rebuilding it mid-request would leave two pools where the
 * deployment budgeted one.
 *
 * The memoised Better Auth instance is deliberately left alone. It is keyed on
 * the configuration digest, so a re-read that returns the SAME configuration
 * reuses it, and one that returns a different configuration replaces it — which
 * is the property that keeps a rotated client secret from surviving in memory.
 */
export function invalidateAuthConfig(): void {
  snapshot = undefined;
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
      /**
       * This configuration is past `AUTH_CONFIG_SNAPSHOT_TTL_MS` and could not be
       * re-read — no bootstrap credential, or Moira is unreachable (issue #152).
       *
       * It is still SERVED, because the alternative is taking sign-in down over a
       * backend blip, and because on the credential-less path the old
       * configuration is the only one anybody could sign in with anyway. But it
       * is served with this flag set, and `/login` renders a notice from it.
       * A configuration that cannot take effect until something changes has to
       * say so; #152 is what silence costs.
       *
       * REQUIRED, not optional. A caller that forgets to state it would report
       * "fresh" for a snapshot of unbounded age, which is the defect wearing a
       * field name.
       */
      readonly stale: boolean;
    }
  | { readonly ok: false; readonly resolution: FailedResolution };

/**
 * Read the configuration from Moira with whatever credential `client` carries,
 * and replace the snapshot with the result.
 *
 * A refusal — Moira answered, and the answer contains no resolvable provider —
 * DISCARDS the snapshot rather than keeping it. It is the source of truth saying
 * "this deployment has no sign-in", and continuing to serve a provider the
 * operator has just disabled is the same defect as #152 in the other direction:
 * a change that is saved, acknowledged, and not in effect.
 *
 * A THROW leaves the snapshot alone — a transport failure or a 5xx is not an
 * answer about the configuration, and `loadAuthConfigs` raises rather than
 * returning `ok: false` for both.
 */
async function readAuthConfig(
  client: MoiraClient,
  env: ConsoleEnv,
): Promise<
  | { readonly ok: true; readonly snapshot: AuthConfigSnapshot }
  | { readonly ok: false; readonly resolution: FailedResolution }
> {
  const resolution = await loadAuthConfigs(client, consoleSecretStore(env), env.bffIssuerUrl);
  if (!resolution.ok) {
    snapshot = undefined;
    return { ok: false, resolution };
  }
  const at = clock();
  const fresh: AuthConfigSnapshot = { resolution, resolvedAtMs: at, attemptedAtMs: at };
  snapshot = fresh;
  return { ok: true, snapshot: fresh };
}

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
  const outcome = await readAuthConfig(client, env);
  return outcome.ok ? outcome.snapshot.resolution : outcome.resolution;
}

/** What this process may serve right now, and whether it is known to be current. */
type SnapshotOutcome =
  | { readonly kind: "serve"; readonly snapshot: AuthConfigSnapshot; readonly stale: boolean }
  | { readonly kind: "refuse"; readonly resolution: FailedResolution };

/** The bootstrap deadlock from the header, as a resolution rather than a throw. */
const BOOTSTRAP_DEADLOCK: FailedResolution = {
  ok: false,
  problem: "no_enabled_provider",
  messageKey: CONSOLE_MESSAGE_KEYS.auth_config_unavailable,
  cacheKey: "unavailable",
};

/**
 * The snapshot to serve this request, refreshed if it has aged out.
 *
 * Four states, and the order below is the order they are decided in:
 *
 *   1. nothing held         — the cold path. Unchanged since before #152.
 *   2. held and fresh       — served, no Moira call. The overwhelmingly common case.
 *   3. held, expired, and re-readable — re-read now, so an out-of-band change to
 *      the provider takes effect without a restart.
 *   4. held, expired, NOT re-readable — served with `stale: true`. No bootstrap
 *      credential, a Moira outage, or a retry floor still in force.
 */
async function currentSnapshot(env: ConsoleEnv): Promise<SnapshotOutcome> {
  const held = snapshot;
  const at = clock();

  if (held === undefined) {
    if (env.moiraSystemKey === undefined) {
      // Nothing snapshotted and no bootstrap credential: the deadlock in the
      // header note, reported as itself rather than as an opaque 401 from Moira.
      return { kind: "refuse", resolution: BOOTSTRAP_DEADLOCK };
    }
    const outcome = await readAuthConfig(moiraClientForSetup(env), env);
    return outcome.ok
      ? { kind: "serve", snapshot: outcome.snapshot, stale: false }
      : { kind: "refuse", resolution: outcome.resolution };
  }

  if (at - held.resolvedAtMs < AUTH_CONFIG_SNAPSHOT_TTL_MS) {
    return { kind: "serve", snapshot: held, stale: false };
  }

  // Expired. Whether it can be re-read is a different question from whether it
  // should be served: it is served either way, and the difference is whether the
  // operator is told.
  if (env.moiraSystemKey === undefined) {
    return { kind: "serve", snapshot: held, stale: true };
  }
  if (at - held.attemptedAtMs < AUTH_CONFIG_REFRESH_RETRY_MS) {
    return { kind: "serve", snapshot: held, stale: true };
  }

  try {
    const outcome = await readAuthConfig(moiraClientForSetup(env), env);
    return outcome.ok
      ? { kind: "serve", snapshot: outcome.snapshot, stale: false }
      : { kind: "refuse", resolution: outcome.resolution };
  } catch (error) {
    // Moira is unreachable and this process holds a usable configuration.
    // Refusing sign-in for the duration of a backend blip would be a worse
    // failure than serving what we have — but it is served as STALE, never
    // passed off as current.
    if (!isMoiraRequestError(error)) throw error;
    snapshot = { ...held, attemptedAtMs: clock() };
    return { kind: "serve", snapshot: held, stale: true };
  }
}

/**
 * The Better Auth instance for the current configuration.
 *
 * Never throws for a *configuration* problem — those come back as
 * `{ ok: false }` with a message key, because "no provider is enabled yet" is
 * the normal first-run state and not an error condition.
 */
export async function consoleRuntime(env: ConsoleEnv = consoleEnv()): Promise<ConsoleRuntime> {
  const outcome = await currentSnapshot(env);
  if (outcome.kind === "refuse") return { ok: false, resolution: outcome.resolution };

  const { stale } = outcome;
  const { configs, problems, cacheKey } = outcome.snapshot.resolution;

  if (cachedAuth !== undefined && cachedAuth.cacheKey === cacheKey) {
    return { ok: true, auth: cachedAuth.auth, configs, problems, stale };
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
  return { ok: true, auth, configs, problems, stale };
}
