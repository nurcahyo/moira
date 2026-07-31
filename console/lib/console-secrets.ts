// @server-only
//
// D7: the console owns the OAuth client secret. Moira never stores it and never
// returns it.
//
// ============================================================================
// WHY THE SECRET LIVES HERE AND NOT IN MOIRA
// ============================================================================
//
// Better Auth needs the PLAINTEXT client secret in console process memory to run
// the OAuth code exchange. Moira's secret envelope is write-only by design, and
// its load-bearing invariant is that a decrypted secret never crosses a network
// boundary. A Moira read-back endpoint would have broken that invariant for
// every secret Moira holds, not just this one; the decision (CONVENTIONS §0 D7)
// was that preserving it is worth the cost of a second configuration store.
//
// The Moira endpoint that would have rotated a provider's secret therefore DOES
// NOT EXIST, and must not be reintroduced. Rotation is a console operation:
// `put()` below is the whole of it. `server-only-guards.test.ts` scans the tree
// for that endpoint's name as a bare literal — comments included — so this note
// deliberately does not spell it.
//
// The unavoidable cost is two stores that can drift: Moira holds `client_id`,
// the console holds the secret for that `client_id`. `SealedClientSecret`
// records the `client_id` the secret was sealed *against* and binds it into the
// AEAD's additional data, so a secret that no longer matches the Moira row
// cannot be silently used — it fails to open, loudly, instead of producing an
// `invalid_client` from the IdP that reads as a network problem.
import "server-only";

import { createCipheriv, createDecipheriv, randomBytes, timingSafeEqual } from "node:crypto";

/* -------------------------------------------------------------------------- */
/* Errors                                                                     */
/* -------------------------------------------------------------------------- */

/**
 * The sealed secret could not be opened.
 *
 * Deliberately carries no detail about *why*: the distinction between "wrong
 * key", "tampered ciphertext" and "wrong client_id" is exactly the oracle an
 * attacker with write access to the store would want. The cause is logged
 * server-side by the caller if it is logged at all.
 */
export class SecretEnvelopeError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "SecretEnvelopeError";
  }
}

/* -------------------------------------------------------------------------- */
/* The envelope                                                               */
/* -------------------------------------------------------------------------- */

/** Bumped only when the ciphertext layout changes. Checked on open. */
export const SECRET_ENVELOPE_VERSION = 1;

const ALGORITHM = "aes-256-gcm";
const IV_BYTES = 12; // GCM's canonical nonce length.
const TAG_BYTES = 16;

/**
 * What is safe to persist. Nothing here is a secret on its own — the ciphertext
 * is useless without the content-encryption key, which lives in the environment.
 */
export interface SealedClientSecret {
  readonly version: number;
  /** Base64url. */
  readonly iv: string;
  /** Base64url. Includes the GCM tag as its final 16 bytes. */
  readonly ciphertext: string;
  /**
   * The Moira `client_id` this secret belongs to, in the clear.
   *
   * Also bound into the AEAD as additional authenticated data, so it cannot be
   * edited in the store without the open failing. Storing it in the clear as
   * well is what lets a drift check compare against Moira's row without
   * decrypting anything.
   */
  readonly clientId: string;
  /** RFC 3339. Feeds the auth-config cache key. */
  readonly updatedAt: string;
}

/**
 * Additional authenticated data: the provider row's identity.
 *
 * Binding `(providerId, clientId)` means a sealed secret is only openable in the
 * context it was sealed for. Copying a row between providers, or editing the
 * stored `client_id` to point at a different Moira row, produces an open
 * failure rather than a successful decrypt of the wrong secret.
 */
function additionalData(providerId: string, clientId: string): Buffer {
  return Buffer.from(`moira-console/v${SECRET_ENVELOPE_VERSION}/${providerId}/${clientId}`, "utf8");
}

function assertKey(key: Buffer): void {
  if (key.byteLength !== 32) {
    throw new SecretEnvelopeError(
      `the content-encryption key must be 32 bytes, got ${key.byteLength}`,
    );
  }
}

/** Seal a plaintext OAuth client secret for storage. */
export function sealClientSecret(
  key: Buffer,
  providerId: string,
  clientId: string,
  plaintext: string,
  now: Date = new Date(),
): SealedClientSecret {
  assertKey(key);
  if (plaintext === "") {
    throw new SecretEnvelopeError("refusing to seal an empty client secret");
  }
  const iv = randomBytes(IV_BYTES);
  const cipher = createCipheriv(ALGORITHM, key, iv, { authTagLength: TAG_BYTES });
  cipher.setAAD(additionalData(providerId, clientId));
  const body = Buffer.concat([cipher.update(plaintext, "utf8"), cipher.final()]);
  const tag = cipher.getAuthTag();
  return {
    version: SECRET_ENVELOPE_VERSION,
    iv: iv.toString("base64url"),
    ciphertext: Buffer.concat([body, tag]).toString("base64url"),
    clientId,
    updatedAt: now.toISOString(),
  };
}

/** Open a sealed secret. Throws `SecretEnvelopeError` on any failure. */
export function openClientSecret(
  key: Buffer,
  providerId: string,
  sealed: SealedClientSecret,
): string {
  assertKey(key);
  if (sealed.version !== SECRET_ENVELOPE_VERSION) {
    throw new SecretEnvelopeError(
      `unsupported secret envelope version ${sealed.version}; this build reads version ` +
        `${SECRET_ENVELOPE_VERSION}`,
    );
  }
  const iv = Buffer.from(sealed.iv, "base64url");
  const combined = Buffer.from(sealed.ciphertext, "base64url");
  if (iv.byteLength !== IV_BYTES || combined.byteLength <= TAG_BYTES) {
    throw new SecretEnvelopeError("the stored client secret is malformed");
  }
  const body = combined.subarray(0, combined.byteLength - TAG_BYTES);
  const tag = combined.subarray(combined.byteLength - TAG_BYTES);
  try {
    const decipher = createDecipheriv(ALGORITHM, key, iv, { authTagLength: TAG_BYTES });
    decipher.setAAD(additionalData(providerId, sealed.clientId));
    decipher.setAuthTag(tag);
    return Buffer.concat([decipher.update(body), decipher.final()]).toString("utf8");
  } catch {
    throw new SecretEnvelopeError("the stored client secret could not be opened");
  }
}

/* -------------------------------------------------------------------------- */
/* The store                                                                  */
/* -------------------------------------------------------------------------- */

/**
 * The console's own persistence for OAuth client secrets.
 *
 * Deliberately narrow. There is no `list()` and no `export()`: the only reason
 * to read a secret is to run one code exchange for one provider, and an
 * interface that cannot enumerate secrets is one an accidental debug endpoint
 * cannot dump.
 */
export interface ConsoleSecretStore {
  /** Store (or replace) the secret for a provider row. */
  put(providerId: string, clientId: string, plaintext: string): Promise<void>;
  /** The sealed record, for drift checks and cache keys. Never the plaintext. */
  read(providerId: string): Promise<SealedClientSecret | null>;
  /**
   * The plaintext, for the code exchange only.
   *
   * Returns `null` when nothing is stored — a provider Moira has enabled but the
   * console has no secret for, which is the drift condition D7 makes possible.
   */
  reveal(providerId: string): Promise<string | null>;
  /** Forget a provider's secret. Used when a provider row is deleted. */
  remove(providerId: string): Promise<void>;
  /**
   * The newest `updatedAt` across every stored secret, or `null` when empty.
   * Feeds the auth-config cache key so a rotation invalidates it.
   */
  newestUpdatedAt(): Promise<string | null>;
}

/**
 * In-process store. THE DEVELOPMENT AND TEST PATH ONLY.
 *
 * `PostgresConsoleSecretStore` in `lib/console-secrets-postgres.ts` is the
 * durable implementation of this same interface, and `lib/env.ts` makes
 * `CONSOLE_DATABASE_URL` a hard boot failure under `NODE_ENV=production` — so a
 * deployment cannot reach this class by omission. It stays because a durable
 * store that cannot be substituted makes every test that touches auth need a
 * database, and because `next dev` should not.
 *
 * WHAT USING IT COSTS, so it is not discovered in production:
 *   * secrets do not survive a restart — the operator must re-run the
 *     auth-settings step;
 *   * a multi-replica deployment has per-replica secrets, so sign-in works only
 *     on the replica that was set up.
 *
 * The envelope functions above are shared by both implementations verbatim:
 * there is exactly one place in this tree where a client secret is encrypted or
 * decrypted, and it is here.
 */
export class InMemoryConsoleSecretStore implements ConsoleSecretStore {
  readonly #key: Buffer;
  readonly #rows = new Map<string, SealedClientSecret>();
  readonly #now: () => Date;

  constructor(key: Buffer, now: () => Date = () => new Date()) {
    assertKey(key);
    this.#key = key;
    this.#now = now;
  }

  async put(providerId: string, clientId: string, plaintext: string): Promise<void> {
    this.#rows.set(providerId, sealClientSecret(this.#key, providerId, clientId, plaintext, this.#now()));
  }

  async read(providerId: string): Promise<SealedClientSecret | null> {
    return this.#rows.get(providerId) ?? null;
  }

  async reveal(providerId: string): Promise<string | null> {
    const sealed = this.#rows.get(providerId);
    if (sealed === undefined) return null;
    return openClientSecret(this.#key, providerId, sealed);
  }

  async remove(providerId: string): Promise<void> {
    this.#rows.delete(providerId);
  }

  async newestUpdatedAt(): Promise<string | null> {
    let newest: string | null = null;
    for (const row of this.#rows.values()) {
      if (newest === null || row.updatedAt > newest) newest = row.updatedAt;
    }
    return newest;
  }
}

/* -------------------------------------------------------------------------- */
/* Drift                                                                      */
/* -------------------------------------------------------------------------- */

/**
 * The two-store drift conditions D7 makes possible.
 *
 * Mandatory, not advisory: D7's whole cost is that these states are reachable,
 * so they have to be nameable. Anything that resolves to a value other than
 * `in_sync` means the console cannot complete a code exchange, and the operator
 * needs to know which half is wrong.
 */
export type ConsoleSecretDrift =
  /** Moira has an enabled provider; the console has a matching secret. */
  | "in_sync"
  /** Moira has the provider; the console has no secret for it. */
  | "console_secret_missing"
  /** Both exist, but the console sealed its secret against a different client_id. */
  | "client_id_mismatch"
  /** The Moira row carries no `client_id`, so there is nothing to bind to. */
  | "moira_client_id_missing";

/**
 * Compare a Moira provider row against the console's stored secret.
 *
 * Compares `client_id` in constant time. Not because a `client_id` is a secret —
 * it is not — but because this function runs on the sign-in path and a
 * length-or-content-dependent comparison here is a free side channel for no
 * benefit.
 */
export function classifySecretDrift(
  moiraClientId: string | null | undefined,
  sealed: SealedClientSecret | null,
): ConsoleSecretDrift {
  if (moiraClientId === null || moiraClientId === undefined || moiraClientId === "") {
    return "moira_client_id_missing";
  }
  if (sealed === null) return "console_secret_missing";
  const a = Buffer.from(moiraClientId, "utf8");
  const b = Buffer.from(sealed.clientId, "utf8");
  if (a.byteLength !== b.byteLength || !timingSafeEqual(a, b)) return "client_id_mismatch";
  return "in_sync";
}

/** Console i18n keys for each drift state. `in_sync` has no message. */
export const CONSOLE_SECRET_DRIFT_MESSAGE_KEYS: Readonly<
  Record<Exclude<ConsoleSecretDrift, "in_sync">, string>
> = {
  console_secret_missing: "console.error.oauth_client_secret_missing",
  client_id_mismatch: "console.error.oauth_client_id_drifted",
  moira_client_id_missing: "console.error.moira_provider_client_id_missing",
};
