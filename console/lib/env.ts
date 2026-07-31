// @server-only
//
// The console's runtime configuration, read once and validated eagerly.
//
// SCOPE. This module holds *deployment* configuration only — where Moira is,
// what origin the console serves on, which key seals the console-owned OAuth
// client secret. The auth provider's own configuration (issuer, client id,
// scopes, allowed email domains) is NOT here: per §7.2 it is read at runtime
// from Moira's DB-backed settings, so that an operator can change it without a
// redeploy. `lib/auth-config.ts` is where that happens.
//
// WHY EAGER VALIDATION. Every value here is load-bearing for a security
// property, and every one of them fails late and confusingly if it is wrong:
// a missing `CONSOLE_SECRET_ENCRYPTION_KEY` surfaces as a decrypt failure on the
// first sign-in, a wrong `MOIRA_ADMIN_API_AUDIENCE` surfaces as a 401 from Moira
// with no local signal at all. Failing at boot, naming the variable, is the
// whole point.
import "server-only";

import { createHash } from "node:crypto";

/* -------------------------------------------------------------------------- */
/* Errors                                                                     */
/* -------------------------------------------------------------------------- */

/** Configuration is missing or invalid. Always fatal; never retried. */
export class ConsoleConfigError extends Error {
  readonly problems: readonly string[];

  constructor(problems: readonly string[]) {
    super(`console configuration is invalid:\n  - ${problems.join("\n  - ")}`);
    this.name = "ConsoleConfigError";
    this.problems = problems;
  }
}

/* -------------------------------------------------------------------------- */
/* Shape                                                                      */
/* -------------------------------------------------------------------------- */

export interface ConsoleEnv {
  /** `production` disables every dev escape hatch. */
  readonly nodeEnv: "development" | "test" | "production";
  /** Moira's admin API base, e.g. `https://moira.internal`. No trailing slash. */
  readonly moiraBaseUrl: string;
  /**
   * The bootstrap system key. Present only while the deployment is unclaimed;
   * an operator is expected to remove it once setup completes, and the console
   * must keep working without it.
   */
  readonly moiraSystemKey: string | undefined;
  /** The console's own public origin. Better Auth's `baseURL`. */
  readonly consoleOrigin: string;
  /**
   * `iss` on every JWT the console mints, and the `issuer` string registered in
   * `trusted_jwt_issuers`. Defaults to `consoleOrigin`; separable because a
   * deployment may sit behind an origin it does not want to name as its issuer.
   */
  readonly bffIssuerUrl: string;
  /**
   * The JWKS document Moira fetches to verify those JWTs. Derived from
   * `consoleOrigin` and the Better Auth mount path — never independently
   * configurable, because a `jwks_url` that does not point at the document the
   * console actually serves is a deployment that authenticates nothing.
   */
  readonly jwksUrl: string;
  /** `aud`. Must be non-empty: an empty `expected_audiences` makes Moira skip
   *  audience validation entirely, which is strictly worse than no audience. */
  readonly adminApiAudience: string;
  /**
   * Better Auth's signing/encryption secret for session material.
   *
   * ALSO the key that encrypts the jwt plugin's ES256 PRIVATE key before it is
   * written to the `jwks` table (better-auth 1.6.25 `plugins/jwt/utils.ts`:
   * encryption is on unless `jwks.disablePrivateKeyEncryption` is set, and this
   * console does not set it).
   *
   * ROTATING THIS AGAINST A DURABLE DATABASE IS A DELIBERATE OPERATION, not a
   * config change. It used to be worse than that (finding F17): `getJwks`
   * served the plaintext `publicKey` column, so the JWKS document was UNCHANGED
   * and Moira's cached copy stayed valid, while `signJWT` could no longer
   * decrypt the private half — and better-auth does not regenerate, because a
   * key that cannot be decrypted is not a missing key. The console kept
   * publishing a JWKS it could not sign for, silently.
   *
   * `lib/jwks-signable.ts` now makes that state a loud `503` instead: the
   * console refuses to publish a key it cannot sign with. Putting the previous
   * value back is a COMPLETE recovery (same `kid`, Moira's cache untouched);
   * rotating on purpose still means deleting the `jwks` rows in the same
   * operation. Runbook in `docs/console-storage.md`; both paths are exercised
   * in `tests/integration/console-jwks-stability.test.ts`.
   */
  readonly betterAuthSecret: string;
  /** 32 raw bytes. Seals the console-owned OAuth client secret at rest (D7). */
  readonly secretEncryptionKey: Buffer;
  /**
   * The console's OWN PostgreSQL database. Never Moira's — see
   * `lib/console-db.ts` for why sharing one database is not an option.
   *
   * `undefined` is permitted outside production and means the ephemeral path:
   * Better Auth's `memoryAdapter` plus `InMemoryConsoleSecretStore`, which is
   * what keeps `bun test` and `next dev` runnable without a database. Under
   * `NODE_ENV=production` it is a hard boot failure, because the ephemeral path
   * silently costs secret durability, a stable JWKS, and `replicaCount > 1` —
   * none of which announces itself when it is missing.
   */
  readonly consoleDatabaseUrl: string | undefined;
  /**
   * Permits `http://` origins and loopback hosts. Dev fixtures only.
   *
   * Mirrors the shape of Moira's own `auth.jwks.allow_insecure_dev_urls`, and
   * like it, is a hard failure under `NODE_ENV=production`. The symmetry is
   * deliberate: the console's `jwks_url` is validated by Moira's SSRF policy, so
   * a console that allows itself an insecure origin in production would register
   * a `jwks_url` Moira must refuse anyway — better to fail here, at boot, with
   * the variable named.
   */
  readonly allowInsecureUrls: boolean;
}

/** Where the Better Auth handler is mounted. Fixed; the route file mirrors it. */
export const AUTH_BASE_PATH = "/api/auth";

/**
 * The JWKS path. `/.well-known/jwks.json` rather than Better Auth's default
 * `/jwks`, because the console publishes it as a deployment-level document and
 * `.well-known` is where an operator debugging Moira's fetch will look first.
 */
export const AUTH_JWKS_PATH = "/.well-known/jwks.json";

/* -------------------------------------------------------------------------- */
/* Parsing                                                                    */
/* -------------------------------------------------------------------------- */

export type EnvSource = Readonly<Record<string, string | undefined>>;

function required(source: EnvSource, name: string, problems: string[]): string {
  const value = source[name];
  if (value === undefined || value.trim() === "") {
    problems.push(`${name} is required and was empty`);
    return "";
  }
  return value.trim();
}

function optional(source: EnvSource, name: string): string | undefined {
  const value = source[name];
  if (value === undefined || value.trim() === "") return undefined;
  return value.trim();
}

function trimTrailingSlash(value: string): string {
  return value.replace(/\/+$/, "");
}

/**
 * Absolute-URL check.
 *
 * Deliberately the same shape as Moira's `validate_https_url`
 * (`src/application/auth_settings.rs`): scheme and non-empty host, nothing more.
 * Matching it means a URL this console accepts is a URL Moira's provider path
 * accepts — including `https://localhost:PORT`, which is exactly what makes a
 * TLS-terminated mock IdP usable as a fixture. Note that Moira's *jwt-issuer*
 * path is stricter (a full SSRF check that rejects loopback), which is why the
 * fixture needs `MOIRA_AUTH__JWKS__ALLOW_INSECURE_DEV_URLS=true` and this check
 * alone is not sufficient evidence that a URL will be accepted everywhere.
 */
function checkAbsoluteUrl(
  value: string,
  name: string,
  allowInsecure: boolean,
  problems: string[],
): void {
  if (value === "") return; // already reported as missing
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    problems.push(`${name} is not an absolute URL: ${value}`);
    return;
  }
  if (parsed.host === "") {
    problems.push(`${name} has no host: ${value}`);
    return;
  }
  if (parsed.protocol !== "https:" && !(allowInsecure && parsed.protocol === "http:")) {
    problems.push(
      `${name} must be an https URL (got ${parsed.protocol}//). ` +
        "Set CONSOLE_ALLOW_INSECURE_URLS=true for a local fixture; it is refused in production.",
    );
  }
}

/**
 * Validate `CONSOLE_DATABASE_URL`.
 *
 * Shape only — no connection is attempted, because boot-time validation must
 * not depend on a database being reachable at that instant. A typo'd scheme or
 * a DSN with no database name is caught here; an unreachable host surfaces on
 * first use, where it can be retried.
 *
 * Every message below prints the *variable name*, never the value: a DSN
 * carries its password inline, and a `ConsoleConfigError` is printed to a log.
 */
function checkDatabaseUrl(
  value: string | undefined,
  nodeEnv: ConsoleEnv["nodeEnv"],
  problems: string[],
): string | undefined {
  if (value === undefined) {
    if (nodeEnv === "production") {
      problems.push(
        "CONSOLE_DATABASE_URL is required in production. Without it the console runs on " +
          "in-memory storage: the OAuth client secret and the Better Auth ES256 signing key " +
          "are lost on every restart, and a second replica publishes a different JWKS. " +
          "Point it at the console's OWN database (never Moira's) and apply " +
          "`console/db/migrations` with `bun run db:migrate`.",
      );
    }
    return undefined;
  }

  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    problems.push("CONSOLE_DATABASE_URL is not a valid connection string URL");
    return undefined;
  }
  if (parsed.protocol !== "postgres:" && parsed.protocol !== "postgresql:") {
    problems.push(
      `CONSOLE_DATABASE_URL must be a postgres:// or postgresql:// URL (got ${parsed.protocol}//)`,
    );
    return undefined;
  }
  if (parsed.hostname === "") {
    problems.push("CONSOLE_DATABASE_URL has no host");
    return undefined;
  }
  if (parsed.pathname.replace(/^\//, "") === "") {
    problems.push(
      "CONSOLE_DATABASE_URL names no database. It must be the console's own database, " +
        "separate from Moira's — the two keep independent migration ledgers.",
    );
    return undefined;
  }
  return value;
}

function parseNodeEnv(value: string | undefined): ConsoleEnv["nodeEnv"] {
  if (value === "production" || value === "test") return value;
  return "development";
}

function parseBoolean(value: string | undefined): boolean {
  return value === "true" || value === "1";
}

/**
 * Decode the 32-byte content-encryption key.
 *
 * Base64 or base64url, and length-checked: a short key is not padded, it is
 * rejected. AES-256-GCM with a 12-byte key would be a silently weaker cipher.
 */
function parseEncryptionKey(raw: string, problems: string[]): Buffer {
  if (raw === "") return Buffer.alloc(0);
  let decoded: Buffer;
  try {
    decoded = Buffer.from(raw, "base64");
  } catch {
    problems.push("CONSOLE_SECRET_ENCRYPTION_KEY is not valid base64");
    return Buffer.alloc(0);
  }
  if (decoded.byteLength !== 32) {
    problems.push(
      `CONSOLE_SECRET_ENCRYPTION_KEY must decode to exactly 32 bytes (got ${decoded.byteLength}). ` +
        "Generate one with: openssl rand -base64 32",
    );
    return Buffer.alloc(0);
  }
  return decoded;
}

/**
 * Names that must never be exposed to the browser.
 *
 * Next.js inlines any `NEXT_PUBLIC_*` variable into the client bundle at build
 * time. A deployment that prefixes one of these — plausibly, while debugging why
 * a value "isn't visible" — ships the console's signing secret to every visitor.
 * The check is on the *prefixed* name, so it fires on the mistake itself rather
 * than on its consequences.
 */
export const NEVER_PUBLIC_ENV_NAMES = [
  "MOIRA_SYSTEM_KEY",
  "BETTER_AUTH_SECRET",
  "CONSOLE_SECRET_ENCRYPTION_KEY",
  "CONSOLE_OAUTH_CLIENT_SECRET",
  // A DSN carries the database password inline. Its name matches none of the
  // SECRET/KEY/TOKEN patterns the leak harness harvests on, so it has to be
  // named here explicitly — see `e2e/support/secrets.ts`, which extracts the
  // password half as a needle for the same reason.
  "CONSOLE_DATABASE_URL",
] as const;

function checkNothingIsPublic(source: EnvSource, problems: string[]): void {
  for (const name of NEVER_PUBLIC_ENV_NAMES) {
    if (source[`NEXT_PUBLIC_${name}`] !== undefined) {
      problems.push(
        `NEXT_PUBLIC_${name} is set. Next.js inlines NEXT_PUBLIC_* into the browser bundle; ` +
          "this value must never leave the server.",
      );
    }
  }
}

/**
 * Validate and normalise the environment.
 *
 * Pure: takes the source explicitly so tests exercise the real function rather
 * than a re-implementation of it.
 */
export function readConsoleEnv(source: EnvSource): ConsoleEnv {
  const problems: string[] = [];
  const nodeEnv = parseNodeEnv(source["NODE_ENV"]);

  const allowInsecureRequested = parseBoolean(source["CONSOLE_ALLOW_INSECURE_URLS"]);
  if (allowInsecureRequested && nodeEnv === "production") {
    problems.push(
      "CONSOLE_ALLOW_INSECURE_URLS must be false in production — it is a fixture-only knob",
    );
  }
  const allowInsecureUrls = allowInsecureRequested && nodeEnv !== "production";

  const moiraBaseUrl = trimTrailingSlash(required(source, "MOIRA_API_URL", problems));
  checkAbsoluteUrl(moiraBaseUrl, "MOIRA_API_URL", allowInsecureUrls, problems);

  const consoleOrigin = trimTrailingSlash(required(source, "CONSOLE_PUBLIC_ORIGIN", problems));
  checkAbsoluteUrl(consoleOrigin, "CONSOLE_PUBLIC_ORIGIN", allowInsecureUrls, problems);

  const bffIssuerUrl = trimTrailingSlash(
    optional(source, "MOIRA_BFF_ISSUER_URL") ?? consoleOrigin,
  );
  checkAbsoluteUrl(bffIssuerUrl, "MOIRA_BFF_ISSUER_URL", allowInsecureUrls, problems);

  const adminApiAudience = required(source, "MOIRA_ADMIN_API_AUDIENCE", problems);
  const betterAuthSecret = required(source, "BETTER_AUTH_SECRET", problems);
  if (betterAuthSecret !== "" && betterAuthSecret.length < 32) {
    problems.push("BETTER_AUTH_SECRET must be at least 32 characters");
  }

  const secretEncryptionKey = parseEncryptionKey(
    required(source, "CONSOLE_SECRET_ENCRYPTION_KEY", problems),
    problems,
  );

  const consoleDatabaseUrl = checkDatabaseUrl(
    optional(source, "CONSOLE_DATABASE_URL"),
    nodeEnv,
    problems,
  );

  checkNothingIsPublic(source, problems);

  if (problems.length > 0) throw new ConsoleConfigError(problems);

  return {
    nodeEnv,
    moiraBaseUrl,
    moiraSystemKey: optional(source, "MOIRA_SYSTEM_KEY"),
    consoleOrigin,
    bffIssuerUrl,
    jwksUrl: `${consoleOrigin}${AUTH_BASE_PATH}${AUTH_JWKS_PATH}`,
    adminApiAudience,
    betterAuthSecret,
    secretEncryptionKey,
    consoleDatabaseUrl,
    allowInsecureUrls,
  };
}

/* -------------------------------------------------------------------------- */
/* Process-wide accessor                                                      */
/* -------------------------------------------------------------------------- */

let cached: ConsoleEnv | undefined;

/** The validated environment for this process. Throws on the first call if invalid. */
export function consoleEnv(): ConsoleEnv {
  cached ??= readConsoleEnv(process.env);
  return cached;
}

/** Test seam. Never called from shipped code paths. */
export function resetConsoleEnvForTests(): void {
  cached = undefined;
}

/**
 * A stable, non-reversible fingerprint of a secret, safe to log.
 *
 * Used to answer "did the secret change?" in logs and in the drift check without
 * ever writing the secret itself. Truncated to 12 hex characters: enough to
 * distinguish values in practice, far too little to attack the preimage.
 */
export function secretFingerprint(value: string): string {
  return createHash("sha256").update(value, "utf8").digest("hex").slice(0, 12);
}
