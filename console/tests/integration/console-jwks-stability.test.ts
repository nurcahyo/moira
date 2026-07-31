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
//
// ============================================================================
// AND THE SECOND HALF: FINDING F17, RAISED HERE AND NOW CLOSED HERE
// ============================================================================
//
// A stable key pair is not enough either. `BETTER_AUTH_SECRET` also ENCRYPTS
// the private half, so rotating it split the pair: `getJwks` kept serving the
// plaintext `publicKey` column while `signJWT` could no longer decrypt. This
// file used to PIN that — it asserted the published document was unchanged and
// that signing raised "Failed to decrypt private key" — which documented the
// defect and guarded nothing.
//
// The lower half of the file now asserts the property instead:
//
//     the console never advertises a key it cannot sign with
//
// against `lib/jwks-signable.ts`, over a real socket, with the minted token
// verified against the served document by `jose`. See the block comment above
// those tests for why a joint assertion is the only kind that can see F17.
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

  /* ------------------------------------------------------------------------ */
  /* FINDING F17 — closed                                                     */
  /* ------------------------------------------------------------------------ */

  // ==========================================================================
  // WHAT WAS WRONG, AND WHAT THE MECHANISM IS
  // ==========================================================================
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
  // So rotating BETTER_AUTH_SECRET against a durable database USED TO produce
  // the worst-shaped failure available: the JWKS document unchanged and still
  // 200ing, Moira's cached copy still valid, sign-in still working — and every
  // Moira-bound token rejected, with nothing about the symptom pointing at the
  // cause. Naive intuition says "rotating the secret regenerates the key pair".
  // It does not: `signJWT` mints a new pair only when there is NO key or it has
  // EXPIRED, and an undecryptable row is neither.
  //
  // `lib/jwks-signable.ts` closes it at the one seam both halves read through
  // (`plugins/jwt/adapter.mjs` routes `getAllKeys` AND `getLatestKey` through
  // `options.adapter.getJwks`), so the document and the signer cannot disagree.
  //
  // ==========================================================================
  // WHY THESE ASSERT ON A JOINT PROPERTY AND NOT ON TWO SEPARATE ANSWERS
  // ==========================================================================
  //
  // The version of this file that raised F17 asked the two questions
  // separately — "what does `getJwks` return" and "does `signJWT` work" — and
  // that is exactly why it could only PIN the defect and never guard against
  // it: neither answer alone is remarkable. A 200 JWKS is normal. A signing
  // failure is normal. The defect is the CONJUNCTION.
  //
  // So `attestConsole` asks both of one process, over a real socket, and
  // verifies the minted token against the served document with `jose` — Moira's
  // own verification path. The invariant asserted below is
  //
  //     published ≠ ∅  ⟹  a token verifies against what was published
  //
  // which is false in the broken arrangement and true in the fixed one, and
  // which no single-sided assertion can express.

  interface Attestation {
    readonly jwksStatus: number;
    readonly jwksBody: unknown;
    readonly jwksError: string | null;
    readonly publishedKids: string[];
    readonly token: string | null;
    readonly signError: string | null;
    readonly verifiedKid: string | null;
    readonly verifyError: string | null;
  }

  async function attestConsole(secret: string, dsn?: string): Promise<Attestation> {
    const result = await runProbe([
      "attest",
      ...(dsn === undefined ? [] : ["--dsn", dsn]),
      "--auth-secret",
      secret,
      "--encryption-key",
      ENCRYPTION_KEY,
    ]);
    return result as unknown as Attestation;
  }

  /**
   * THE PROPERTY. Never a key it cannot sign with.
   *
   * Vacuous when nothing is published, deliberately — "publish nothing" is a
   * legitimate refusal. Every caller therefore states separately, and
   * non-vacuously, whether it EXPECTED something to be published; a suite that
   * only called this would be satisfied by a console that never serves a key at
   * all.
   */
  function expectNeverAdvertisesWhatItCannotSign(attestation: Attestation, when: string): void {
    expect(attestation.jwksError, `${when}: the JWKS document could not be fetched at all`).toBe(
      null,
    );
    if (attestation.publishedKids.length === 0) return;

    expect(
      attestation.signError,
      `${when}: the console PUBLISHED ${JSON.stringify(attestation.publishedKids)} and then ` +
        "failed to sign. That is finding F17 exactly: a JWKS endpoint that looks healthy while " +
        "every token the console mints is rejected, from a console that is up and serving " +
        "sign-in normally. Since wave 4B this takes down all N provider issuers at once — they " +
        "share one key pair and one JWKS URL.",
    ).toBe(null);
    expect(attestation.token, `${when}: no token was minted`).toBeString();
    expect(
      attestation.verifyError,
      `${when}: the minted token does NOT verify against the document the console just served. ` +
        "Moira resolves the signing key by `kid` out of the fetched JWKS, so this is a total " +
        "admin-API outage that no health check can see.",
    ).toBe(null);
    expect(
      attestation.verifiedKid,
      `${when}: the token was signed with a kid that is not in the published document`,
    ).toBe(attestation.publishedKids[0] ?? null);
  }

  test("F17 — a BETTER_AUTH_SECRET rotation is REFUSED, not silently published", async () => {
    const dsn = testDatabaseUrl();
    await resetConsoleTestDatabase(pool);
    const rotated = "a-completely-different-better-auth-secret-0af3";

    // ---- healthy, before anything is rotated -------------------------------
    const before = await attestConsole(AUTH_SECRET, dsn);
    expectNeverAdvertisesWhatItCannotSign(before, "before the rotation");
    // NON-VACUITY. Without this the assertion above is satisfied by a console
    // that publishes nothing, which is the failure mode a filtering mechanism
    // is most likely to have.
    expect(before.jwksStatus, "the console did not serve its JWKS at all").toBe(200);
    expect(before.publishedKids, "exactly one key pair backs the console").toHaveLength(1);

    // ---- the rotation ------------------------------------------------------
    const after = await attestConsole(rotated, dsn);
    expectNeverAdvertisesWhatItCannotSign(after, "after rotating BETTER_AUTH_SECRET");

    // THE MECHANISM, pinned separately from the property. Returning an empty
    // key set here would ALSO satisfy the property — better-auth would mint a
    // fresh pair and publish that — and it is a different decision with
    // different costs (see `lib/jwks-signable.ts` and docs/console-storage.md).
    // The choice recorded there is to refuse, so the refusal is asserted.
    expect(
      after.jwksStatus,
      "the JWKS endpoint answered something other than a 503 after the rotation. A 200 is " +
        "finding F17; anything else means the mechanism changed and the decision record in " +
        "docs/console-storage.md is now describing something that is not shipped.",
    ).toBe(503);
    expect((after.jwksBody as { code?: string }).code).toBe("JWKS_KEY_UNSIGNABLE");
    expect(after.publishedKids).toHaveLength(0);
    expect(String(after.signError ?? ""), "minting must refuse too, not just publishing").toContain(
      "refusing to publish",
    );

    // ---- and the key material SURVIVED the refusal -------------------------
    //
    // The whole argument for refusing rather than regenerating. An operator who
    // supplied the wrong secret by mistake has lost nothing: putting it back is
    // a COMPLETE recovery, with the same `kid`, so Moira's cached JWKS was
    // never invalidated and no token in flight was orphaned. A mechanism that
    // healed itself would have destroyed that by minting a new console
    // identity, and would have done it silently.
    const rows = await pool.query<{ n: string }>('select count(*)::text as n from "jwks"');
    expect(rows.rows[0]?.n, "the refusal must not delete or replace the stored pair").toBe("1");

    const restored = await attestConsole(AUTH_SECRET, dsn);
    expectNeverAdvertisesWhatItCannotSign(restored, "after putting the original secret back");
    expect(restored.jwksStatus).toBe(200);
    expect(
      restored.publishedKids,
      "putting the original BETTER_AUTH_SECRET back did not restore the ORIGINAL key pair, so " +
        "the refusal is not the lossless recovery docs/console-storage.md claims it is",
    ).toEqual(before.publishedKids);
  }, 180_000);

  test("F17 — the documented recovery works: delete the rows, get a new pair", async () => {
    // The one thing an operator must still do by hand, so it is tested rather
    // than described. `docs/console-storage.md` names this SQL.
    const dsn = testDatabaseUrl();
    await resetConsoleTestDatabase(pool);
    const rotated = "yet-another-better-auth-secret-for-the-recovery-path";

    const before = await attestConsole(AUTH_SECRET, dsn);
    expect(before.jwksStatus).toBe(200);
    expect(before.publishedKids).toHaveLength(1);

    const refused = await attestConsole(rotated, dsn);
    expect(refused.jwksStatus, "premise: the rotation is refused").toBe(503);

    await pool.query('delete from "jwks"');

    const recovered = await attestConsole(rotated, dsn);
    expectNeverAdvertisesWhatItCannotSign(recovered, "after the documented recovery");
    expect(recovered.jwksStatus, "the console did not come back after the documented recovery").toBe(
      200,
    );
    expect(recovered.publishedKids).toHaveLength(1);
    expect(
      recovered.publishedKids,
      "the recovery produced the SAME key id, which is impossible unless the probe is reporting " +
        "something other than what the console served",
    ).not.toEqual(before.publishedKids);
  }, 180_000);

  test("CONTROL: the attestation is not a constant — the memory adapter attests healthy too", async () => {
    // Without this the two tests above are consistent with an `attest` command
    // that returns a fixed healthy answer for one secret and a fixed 503 for
    // another. The memory adapter shares every line of the mechanism and none
    // of the durable storage, and it must come back healthy on any secret.
    const memory = await attestConsole("a-memory-adapter-secret-that-is-long-enough-x");
    expectNeverAdvertisesWhatItCannotSign(memory, "on the memory adapter");
    expect(memory.jwksStatus).toBe(200);
    expect(memory.publishedKids).toHaveLength(1);
  }, 120_000);
});
