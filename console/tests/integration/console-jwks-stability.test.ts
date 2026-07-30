// The property Moira's verification actually depends on.
//
// ============================================================================
// WHY THIS IS THE SHARPEST OF THE THREE DURABILITY CLAIMS
// ============================================================================
//
// `console/lib/auth.ts` states the failure in one line: with the memory adapter
// the ES256 key pair is regenerated on every process start, so "the JWKS
// document Moira fetched a minute ago stops verifying newly minted tokens".
//
// That is worse than it sounds. Moira caches the console's JWKS. A console
// restart therefore does not produce an error anyone can act on — it produces
// signature verification failures on every admin call until Moira's cache
// happens to expire, from a console that is up, healthy, and serving sign-in
// normally. Nothing in either process reports the cause.
//
// So this file spawns two genuinely separate `bun` processes and compares the
// PUBLIC HALF of the key, in full. Comparing `kid` alone would pass if the kid
// were derived from something stable while the key itself changed.
//
// The memory adapter is run through the identical probe as the control, and it
// must produce a DIFFERENT key. Without that case, "the keys matched" would be
// consistent with a probe that simply returned a constant.
import { afterAll, beforeAll, expect, test } from "bun:test";

import type { Pool } from "pg";

import {
  DATABASE_TESTS_SKIPPED,
  describeDatabase,
  openConsoleTestDatabase,
  resetConsoleTestDatabase,
  testDatabaseUrl,
} from "../support/console-db";
import { runProbe } from "../support/run-probe";

const ENCRYPTION_KEY = Buffer.alloc(32, 7).toString("base64");
const AUTH_SECRET = "jwks-stability-better-auth-secret-4d0e91bc7a";

let pool: Pool;

beforeAll(async () => {
  if (DATABASE_TESTS_SKIPPED) return;
  pool = await openConsoleTestDatabase();
  await resetConsoleTestDatabase(pool);
});

afterAll(async () => {
  await pool?.end();
});

interface Jwk {
  readonly kid?: string;
  readonly kty?: string;
  readonly crv?: string;
  readonly x?: string;
  readonly y?: string;
  readonly alg?: string;
}

async function jwksFrom(args: readonly string[]): Promise<Jwk[]> {
  const result = await runProbe([
    "jwks",
    "--auth-secret",
    AUTH_SECRET,
    "--encryption-key",
    ENCRYPTION_KEY,
    ...args,
  ]);
  return result["keys"] as Jwk[];
}

describeDatabase("the JWKS key pair", () => {
  test("is identical across two separate processes when the database is durable", async () => {
    const dsn = testDatabaseUrl();

    // First process: virgin `jwks` table, so Better Auth generates the pair and
    // writes it. Then it exits.
    const first = await jwksFrom(["--dsn", dsn]);
    // Second process: nothing in common with the first but the database.
    const second = await jwksFrom(["--dsn", dsn]);

    expect(first.length, "the probe published no key at all").toBeGreaterThan(0);
    expect(first[0]?.kty).toBe("EC");
    expect(first[0]?.crv).toBe("P-256");
    expect(first[0]?.alg).toBe("ES256");

    expect(
      second,
      "the console published a different JWKS after a restart. Every token minted by the " +
        "new process fails verification against the copy Moira already cached, from a " +
        "console that is up and serving sign-in normally.",
    ).toEqual(first);

    // And there is exactly one row backing it, so a restart is not quietly
    // accumulating key pairs that all remain published.
    const rows = await pool.query<{ n: string }>('select count(*)::text as n from "jwks"');
    expect(rows.rows[0]?.n).toBe("1");
  }, 120_000);

  test("CONTROL: the memory adapter publishes a different key each process", async () => {
    const first = await jwksFrom([]);
    const second = await jwksFrom([]);

    expect(first.length).toBeGreaterThan(0);
    expect(second.length).toBeGreaterThan(0);
    expect(
      second[0]?.x,
      "the in-memory control produced the SAME key twice, so the durable test above " +
        "proves nothing — it would pass against any adapter.",
    ).not.toBe(first[0]?.x);
  }, 120_000);

  test("BETTER_AUTH_SECRET is a SECOND key the durable pair depends on", async () => {
    // ========================================================================
    // A finding, verified here rather than assumed.
    // ========================================================================
    //
    // Durability is necessary for a stable JWKS. It is not sufficient. Reading
    // better-auth 1.6.25's `plugins/jwt/utils.ts` and `plugins/jwt/sign.ts`:
    //
    //   * `createJwk` stores `publicKey` as plaintext JSON and `privateKey`
    //     `symmetricEncrypt`ed under `ctx.context.secretConfig` — i.e. under
    //     BETTER_AUTH_SECRET. Encryption is ON unless
    //     `jwks.disablePrivateKeyEncryption` is set, and this console does not
    //     set it;
    //   * `getJwks` serves the `publicKey` column verbatim and never touches
    //     the private half;
    //   * `signJWT` must `symmetricDecrypt` it, and on failure raises
    //     "Failed to decrypt private key...".
    //
    // So rotating BETTER_AUTH_SECRET against a durable database produces the
    // WORST-shaped failure available: the JWKS document is unchanged and still
    // 200s, Moira's cached copy stays valid, sign-in still works — and every
    // attempt to mint a Moira-bound token fails. Nothing about the symptom
    // points at the cause.
    //
    // Naive intuition says "rotating the secret regenerates the key pair".
    // It does not: `signJWT` only regenerates when there is NO key or the key
    // has expired, and the undecryptable row is neither.
    const dsn = testDatabaseUrl();
    await resetConsoleTestDatabase(pool);
    const rotated = "a-completely-different-better-auth-secret-0af3";

    const before = await jwksFrom(["--dsn", dsn]);
    const signedBefore = await runProbe([
      "sign",
      "--dsn",
      dsn,
      "--auth-secret",
      AUTH_SECRET,
      "--encryption-key",
      ENCRYPTION_KEY,
    ]);
    expect(signedBefore["error"], "signing should work with the original secret").toBeNull();
    expect(signedBefore["token"]).toBeString();

    const after = await runProbe([
      "jwks",
      "--dsn",
      dsn,
      "--auth-secret",
      rotated,
      "--encryption-key",
      ENCRYPTION_KEY,
    ]);
    expect(
      (after["keys"] as Jwk[])[0]?.x,
      "the published JWKS is unchanged by the rotation — which is exactly why the failure " +
        "is invisible from outside",
    ).toBe(before[0]?.x);

    const signedAfter = await runProbe([
      "sign",
      "--dsn",
      dsn,
      "--auth-secret",
      rotated,
      "--encryption-key",
      ENCRYPTION_KEY,
    ]);
    expect(
      String(signedAfter["error"] ?? ""),
      "signing succeeded after rotating BETTER_AUTH_SECRET, so this finding no longer holds " +
        "and the runbook in docs/console-storage.md should be revisited",
    ).toContain("Failed to decrypt private key");
    expect(signedAfter["token"]).toBeNull();

    // And the row was NOT replaced: an undecryptable key is not treated as a
    // missing one.
    const rows = await pool.query<{ n: string }>('select count(*)::text as n from "jwks"');
    expect(rows.rows[0]?.n).toBe("1");
  }, 120_000);
});
