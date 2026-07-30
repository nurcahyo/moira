// @server-only
//
// `ConsoleSecretStore` over the console's own PostgreSQL database — the durable
// half of D7.
//
// ============================================================================
// THE INTERFACE IS PLAN 08'S, UNCHANGED
// ============================================================================
//
// `put` / `read` / `reveal` / `remove` / `newestUpdatedAt`, exactly as
// `lib/console-secrets.ts` declares them, and exactly as
// `InMemoryConsoleSecretStore` implements them. Nothing about the envelope
// changes either: this class CALLS `sealClientSecret` and `openClientSecret`
// rather than re-implementing them, so there remains exactly one place in the
// tree where a client secret is encrypted or decrypted, and the AES-256-GCM
// envelope with its AAD binding to `(providerId, clientId)` is the same tested
// code on both paths.
//
// ============================================================================
// WHY THIS IS A SEPARATE MODULE FROM `console-secrets.ts`
// ============================================================================
//
// `console-secrets.ts` is pure `node:crypto` and pulls in no driver. Its unit
// tests, and the static scanners in `tests/unit/architecture/`, read it without
// dragging a database client into their import graph. Putting the SQL here keeps
// that true — and keeps the in-memory implementation genuinely usable, which is
// the point: a durable store that cannot be substituted makes every test that
// touches auth need a database.
//
// ============================================================================
// WHERE THE ENCRYPTION KEY COMES FROM, AND WHAT HAPPENS WITHOUT IT
// ============================================================================
//
// `CONSOLE_SECRET_ENCRYPTION_KEY`: 32 raw bytes, base64, validated at boot by
// `lib/env.ts`. It is NOT in this database and must never be — the ciphertext
// and its key in one place is not encryption at rest, it is an inconvenient
// plaintext.
//
//   absent          the console does not boot. `readConsoleEnv` names the
//                   variable. There is no degraded mode.
//   wrong / rotated every stored row fails to open. `reveal()` throws
//                   `SecretEnvelopeError` — deliberately without saying whether
//                   the cause was the key, the ciphertext or the client id,
//                   since that distinction is an oracle. The operator re-enters
//                   each provider's client secret, which rewrites the row under
//                   the new key. There is no re-encryption path and no key list,
//                   because supporting one means keeping the OLD key available
//                   to the process, which is most of what rotating it was for.
//
// Rotating the key is therefore an operator procedure, not a migration:
// set the new key, restart, re-enter each provider's client secret. With one
// provider that is one form. `docs/console-storage.md` carries the runbook.
import "server-only";

import type { Pool } from "pg";

import {
  openClientSecret,
  sealClientSecret,
  SECRET_ENVELOPE_VERSION,
  SecretEnvelopeError,
  type ConsoleSecretStore,
  type SealedClientSecret,
} from "./console-secrets";

/** The table `db/migrations/0002_console_provider_secret.sql` creates. */
export const PROVIDER_SECRET_TABLE = "console_provider_secret";

interface SecretRow {
  readonly provider_id: string;
  readonly envelope_version: number;
  readonly iv: string;
  readonly ciphertext: string;
  readonly client_id: string;
  readonly updated_at: Date;
}

/**
 * `updated_at` back to the RFC 3339 string the envelope carries.
 *
 * `pg` hands back a `Date` for `timestamptz`; `toISOString()` renormalises to
 * UTC at millisecond precision, which is exactly what `sealClientSecret` wrote
 * with `now.toISOString()`. The round trip has to be exact, because this string
 * feeds `authConfigCacheKey` — a value that changed shape across a restart would
 * rebuild the Better Auth instance on the first request after every deploy.
 */
function toEnvelope(row: SecretRow): SealedClientSecret {
  return {
    version: row.envelope_version,
    iv: row.iv,
    ciphertext: row.ciphertext,
    clientId: row.client_id,
    updatedAt: row.updated_at.toISOString(),
  };
}

/**
 * Durable `ConsoleSecretStore`.
 *
 * Holds a `Pool`, not a `Client`: every method is a single statement, so there
 * is no session state to keep and a pooled connection is returned immediately.
 */
export class PostgresConsoleSecretStore implements ConsoleSecretStore {
  readonly #pool: Pool;
  readonly #key: Buffer;
  readonly #now: () => Date;

  constructor(pool: Pool, key: Buffer, now: () => Date = () => new Date()) {
    // Seal one throwaway value so a wrong-length key fails HERE, at
    // construction, rather than on the first sign-in. `sealClientSecret`
    // performs the same length check the in-memory store's constructor does.
    sealClientSecret(key, "startup-check", "startup-check", "startup-check", new Date(0));
    this.#pool = pool;
    this.#key = key;
    this.#now = now;
  }

  /**
   * Store (or replace) the secret for a provider row.
   *
   * `on conflict do update` because rotation IS this method — D7 leaves the
   * console as the only place a client secret can be replaced, and a rotation
   * that had to delete first would have a window in which sign-in was broken.
   * The whole row is replaced, `client_id` included, so re-pointing a provider
   * at a new OAuth application is the same single call.
   */
  async put(providerId: string, clientId: string, plaintext: string): Promise<void> {
    const sealed = sealClientSecret(this.#key, providerId, clientId, plaintext, this.#now());
    await this.#pool.query(
      `insert into ${PROVIDER_SECRET_TABLE}
         (provider_id, envelope_version, iv, ciphertext, client_id, updated_at)
       values ($1, $2, $3, $4, $5, $6)
       on conflict (provider_id) do update set
         envelope_version = excluded.envelope_version,
         iv               = excluded.iv,
         ciphertext       = excluded.ciphertext,
         client_id        = excluded.client_id,
         updated_at       = excluded.updated_at`,
      [
        providerId,
        sealed.version,
        sealed.iv,
        sealed.ciphertext,
        sealed.clientId,
        sealed.updatedAt,
      ],
    );
  }

  /** The sealed record, for drift checks and cache keys. Never the plaintext. */
  async read(providerId: string): Promise<SealedClientSecret | null> {
    const result = await this.#pool.query<SecretRow>(
      `select provider_id, envelope_version, iv, ciphertext, client_id, updated_at
         from ${PROVIDER_SECRET_TABLE} where provider_id = $1`,
      [providerId],
    );
    const row = result.rows[0];
    return row === undefined ? null : toEnvelope(row);
  }

  /**
   * The plaintext, for the code exchange only.
   *
   * `null` means "nothing stored" — the `console_secret_missing` drift state.
   * A row that exists but cannot be opened is NOT `null`: it throws, because
   * "no secret configured" and "the secret is unreadable" need different
   * operator actions and collapsing them into one would send an operator to
   * re-run setup when the real problem is a rotated encryption key.
   */
  async reveal(providerId: string): Promise<string | null> {
    const sealed = await this.read(providerId);
    if (sealed === null) return null;
    if (sealed.version !== SECRET_ENVELOPE_VERSION) {
      throw new SecretEnvelopeError(
        `unsupported secret envelope version ${sealed.version}; this build reads version ` +
          `${SECRET_ENVELOPE_VERSION}`,
      );
    }
    return openClientSecret(this.#key, providerId, sealed);
  }

  /** Forget a provider's secret. Used when a provider row is deleted. */
  async remove(providerId: string): Promise<void> {
    await this.#pool.query(`delete from ${PROVIDER_SECRET_TABLE} where provider_id = $1`, [
      providerId,
    ]);
  }

  /**
   * The newest `updatedAt` across every stored secret, or `null` when empty.
   *
   * Feeds `authConfigCacheKey`, so a rotation on ANOTHER replica invalidates
   * this replica's cached Better Auth instance on its next resolution. That is
   * the property the in-memory store could not have, and it is half of what
   * makes `replicaCount > 1` safe.
   */
  async newestUpdatedAt(): Promise<string | null> {
    const result = await this.#pool.query<{ newest: Date | null }>(
      `select max(updated_at) as newest from ${PROVIDER_SECRET_TABLE}`,
    );
    const newest = result.rows[0]?.newest ?? null;
    return newest === null ? null : newest.toISOString();
  }
}
