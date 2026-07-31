// `PostgresConsoleSecretStore` against a real PostgreSQL.
//
// ============================================================================
// WHAT "DURABLE" IS ALLOWED TO MEAN HERE
// ============================================================================
//
// Not "write then read". An in-memory `Map` passes that. The claim under test is
// that a secret written by one process is readable by a DIFFERENT process, so
// the writes and the reads happen in separate `bun` children (see
// `tests/support/durability-probe.ts`) with nothing shared between them but the
// database.
//
// The in-memory store is exercised the same way as the CONTROL: it must fail to
// carry a secret across processes. Without that case the suite could not
// distinguish "durable" from "the same object answered twice".
//
// ============================================================================
// THE AT-REST LEAK SURFACE
// ============================================================================
//
// A durable store is a new place a secret can leak, and it is not a place
// `e2e/secret-leak.e2e.ts` can look: that suite scans what the BROWSER sees, and
// this table has no browser surface at all in this wave. So the at-rest scan
// lives here, where a real database is available — every column of every row is
// read back and asserted not to contain the plaintext, its base64, or its hex.
import { afterAll, afterEach, beforeAll, expect, test } from "bun:test";
import { randomBytes } from "node:crypto";

import { Pool } from "pg";

import { createConsolePool } from "../../lib/console-db";
import {
  InMemoryConsoleSecretStore,
  SecretEnvelopeError,
  sealClientSecret,
  type ConsoleSecretStore,
} from "../../lib/console-secrets";
import { PostgresConsoleSecretStore } from "../../lib/console-secrets-postgres";
import {
  DATABASE_TESTS_SKIPPED,
  describeDatabase,
  openConsoleTestDatabase,
  resetConsoleTestDatabase,
  testDatabaseUrl,
} from "../support/console-db";
import { runProbe } from "../support/run-probe";

/** A high-entropy stand-in for an OAuth client secret. */
const PLAINTEXT = "moira-console-durable-secret-3f19bd0c74a8e25691cf40db8a7e3c16";
const PROVIDER = "provider-01J8Z9";
const CLIENT_ID = "client-id-8fb31d";

const KEY = randomBytes(32);
const AUTH_SECRET = "durability-probe-better-auth-secret-9c41ff2ab6";

let pool: Pool;

beforeAll(async () => {
  if (DATABASE_TESTS_SKIPPED) return;
  pool = await openConsoleTestDatabase();
});

afterEach(async () => {
  if (DATABASE_TESTS_SKIPPED) return;
  await resetConsoleTestDatabase(pool);
});

afterAll(async () => {
  await pool?.end();
});

function store(): ConsoleSecretStore {
  return new PostgresConsoleSecretStore(pool, KEY);
}

/* -------------------------------------------------------------------------- */
/* The interface, unchanged                                                   */
/* -------------------------------------------------------------------------- */

describeDatabase("the plan-08 interface, implemented over PostgreSQL", () => {
  test("read and reveal return null for an unknown provider", async () => {
    expect(await store().read("nobody")).toBeNull();
    expect(await store().reveal("nobody")).toBeNull();
    expect(await store().newestUpdatedAt()).toBeNull();
  });

  test("put then read returns the sealed record, never the plaintext", async () => {
    await store().put(PROVIDER, CLIENT_ID, PLAINTEXT);
    const sealed = await store().read(PROVIDER);
    expect(sealed).not.toBeNull();
    expect(sealed?.clientId).toBe(CLIENT_ID);
    expect(sealed?.version).toBe(1);
    expect(JSON.stringify(sealed)).not.toContain(PLAINTEXT);
  });

  test("reveal returns the plaintext", async () => {
    await store().put(PROVIDER, CLIENT_ID, PLAINTEXT);
    expect(await store().reveal(PROVIDER)).toBe(PLAINTEXT);
  });

  test("put replaces — rotation is one call, with no window where nothing is stored", async () => {
    await store().put(PROVIDER, CLIENT_ID, PLAINTEXT);
    await store().put(PROVIDER, CLIENT_ID, "rotated-secret-value-b17e");
    expect(await store().reveal(PROVIDER)).toBe("rotated-secret-value-b17e");

    const count = await pool.query<{ n: string }>(
      "select count(*)::text as n from console_provider_secret where provider_id = $1",
      [PROVIDER],
    );
    expect(count.rows[0]?.n).toBe("1");
  });

  test("re-pointing a provider at a new OAuth application rebinds the client id", async () => {
    await store().put(PROVIDER, CLIENT_ID, PLAINTEXT);
    await store().put(PROVIDER, "client-id-new-4c21", "secret-for-the-new-app-77af");
    const sealed = await store().read(PROVIDER);
    expect(sealed?.clientId).toBe("client-id-new-4c21");
    expect(await store().reveal(PROVIDER)).toBe("secret-for-the-new-app-77af");
  });

  test("remove forgets it", async () => {
    await store().put(PROVIDER, CLIENT_ID, PLAINTEXT);
    await store().remove(PROVIDER);
    expect(await store().read(PROVIDER)).toBeNull();
    expect(await store().reveal(PROVIDER)).toBeNull();
  });

  test("newestUpdatedAt is the max across providers and feeds the cache key", async () => {
    const early = new Date("2026-01-01T00:00:00.000Z");
    const late = new Date("2026-06-01T12:34:56.789Z");
    await new PostgresConsoleSecretStore(pool, KEY, () => early).put("a", "ca", "secret-alpha-01");
    await new PostgresConsoleSecretStore(pool, KEY, () => late).put("b", "cb", "secret-beta-002");
    expect(await store().newestUpdatedAt()).toBe(late.toISOString());
  });

  test("updatedAt round-trips exactly — the cache key must not churn on restart", async () => {
    // `authConfigCacheKey` hashes this string. A round trip that changed its
    // shape (or dropped milliseconds) would rebuild the Better Auth instance on
    // the first request after every deploy, silently.
    const at = new Date("2026-03-04T05:06:07.008Z");
    await new PostgresConsoleSecretStore(pool, KEY, () => at).put(PROVIDER, CLIENT_ID, PLAINTEXT);
    expect((await store().read(PROVIDER))?.updatedAt).toBe("2026-03-04T05:06:07.008Z");
  });
});

/* -------------------------------------------------------------------------- */
/* The envelope's guarantees survive persistence                              */
/* -------------------------------------------------------------------------- */

describeDatabase("the AAD binding still holds through the database", () => {
  test("a wrong content-encryption key cannot open a stored secret", async () => {
    await store().put(PROVIDER, CLIENT_ID, PLAINTEXT);
    const wrongKey = new PostgresConsoleSecretStore(pool, randomBytes(32));
    await expect(wrongKey.reveal(PROVIDER)).rejects.toThrow(SecretEnvelopeError);
  });

  test("editing client_id in the database makes the open fail, not succeed wrongly", async () => {
    // The exact scenario D7 makes possible: Moira's row moves to a new client
    // id and someone "fixes" the console's copy by hand. The AAD binding turns
    // that into a loud failure instead of an `invalid_client` from the IdP that
    // reads as a network problem.
    await store().put(PROVIDER, CLIENT_ID, PLAINTEXT);
    await pool.query("update console_provider_secret set client_id = $1 where provider_id = $2", [
      "client-id-tampered",
      PROVIDER,
    ]);
    await expect(store().reveal(PROVIDER)).rejects.toThrow(SecretEnvelopeError);
  });

  test("copying a row to another provider_id makes the open fail", async () => {
    await store().put(PROVIDER, CLIENT_ID, PLAINTEXT);
    await pool.query(
      `insert into console_provider_secret
         (provider_id, envelope_version, iv, ciphertext, client_id, updated_at)
       select 'other-provider', envelope_version, iv, ciphertext, client_id, updated_at
         from console_provider_secret where provider_id = $1`,
      [PROVIDER],
    );
    await expect(store().reveal("other-provider")).rejects.toThrow(SecretEnvelopeError);
  });

  test("an unknown envelope version is refused rather than guessed", async () => {
    await store().put(PROVIDER, CLIENT_ID, PLAINTEXT);
    await pool.query(
      "update console_provider_secret set envelope_version = 99 where provider_id = $1",
      [PROVIDER],
    );
    await expect(store().reveal(PROVIDER)).rejects.toThrow(/unsupported secret envelope version/);
  });

  test("a wrong-length key is refused at construction, not at first sign-in", () => {
    expect(() => new PostgresConsoleSecretStore(pool, randomBytes(16))).toThrow(
      SecretEnvelopeError,
    );
  });

  test("an empty secret is refused before it reaches the database", async () => {
    await expect(store().put(PROVIDER, CLIENT_ID, "")).rejects.toThrow(SecretEnvelopeError);
    expect(await store().read(PROVIDER)).toBeNull();
  });
});

/* -------------------------------------------------------------------------- */
/* At rest                                                                    */
/* -------------------------------------------------------------------------- */

describeDatabase("nothing readable is written to the database", () => {
  test("no column of any row contains the plaintext in any obvious encoding", async () => {
    await store().put(PROVIDER, CLIENT_ID, PLAINTEXT);

    const rows = await pool.query("select * from console_provider_secret");
    const dump = JSON.stringify(rows.rows);

    const encodings = [
      ["utf8", PLAINTEXT],
      ["base64", Buffer.from(PLAINTEXT, "utf8").toString("base64")],
      ["base64url", Buffer.from(PLAINTEXT, "utf8").toString("base64url")],
      ["hex", Buffer.from(PLAINTEXT, "utf8").toString("hex")],
    ] as const;

    for (const [label, needle] of encodings) {
      expect(dump.includes(needle), `the client secret appears in the table as ${label}`).toBe(
        false,
      );
    }
  });

  test("the content-encryption key is never written to the database", async () => {
    await store().put(PROVIDER, CLIENT_ID, PLAINTEXT);
    // Ciphertext plus its key in one place is not encryption at rest, it is an
    // inconvenient plaintext. The key belongs to CONSOLE_SECRET_ENCRYPTION_KEY
    // and to nothing else.
    const everything = await pool.query("select * from console_provider_secret");
    const dump = JSON.stringify(everything.rows);
    for (const encoding of ["base64", "base64url", "hex"] as const) {
      expect(dump).not.toContain(KEY.toString(encoding));
    }
  });

  test("ciphertext is not deterministic — two seals of one secret differ", async () => {
    // A fresh 12-byte GCM nonce per seal. Identical ciphertext for identical
    // input would let anyone with read access to the table tell that two
    // providers share a client secret, and would be catastrophic under GCM if
    // the nonce were ever reused with the same key.
    const a = sealClientSecret(KEY, PROVIDER, CLIENT_ID, PLAINTEXT);
    const b = sealClientSecret(KEY, PROVIDER, CLIENT_ID, PLAINTEXT);
    expect(a.ciphertext).not.toBe(b.ciphertext);
    expect(a.iv).not.toBe(b.iv);
  });

  test("the schema declares no column that could hold a plaintext secret", async () => {
    const columns = await pool.query<{ column_name: string }>(
      `select column_name from information_schema.columns
        where table_name = 'console_provider_secret'`,
    );
    const offenders = columns.rows
      .map((r) => r.column_name)
      .filter((name) => /(^|_)(secret|password|plaintext|token)($|_)/i.test(name));
    expect(offenders).toEqual([]);
  });
});

/* -------------------------------------------------------------------------- */
/* Durability: the actual claim                                               */
/* -------------------------------------------------------------------------- */

describeDatabase("a secret survives the process that wrote it", () => {
  const dsn = testDatabaseUrl();
  const probeArgs = ["--auth-secret", AUTH_SECRET, "--encryption-key", KEY.toString("base64")];

  test("written by one process, revealed by another", async () => {
    const written = await runProbe([
      "secret-put",
      "--dsn",
      dsn,
      "--provider",
      PROVIDER,
      "--client",
      CLIENT_ID,
      "--secret",
      PLAINTEXT,
      ...probeArgs,
    ]);
    expect(written["updatedAt"]).toBeString();

    // Separate `bun` process. Separate heap, separate module registry, separate
    // pool. The only thing shared is PostgreSQL.
    const read = await runProbe([
      "secret-reveal",
      "--dsn",
      dsn,
      "--provider",
      PROVIDER,
      ...probeArgs,
    ]);
    expect(read["revealed"]).toBe(PLAINTEXT);
    expect(read["clientId"]).toBe(CLIENT_ID);
    expect(read["updatedAt"]).toBe(written["updatedAt"]);
    expect(read["newestUpdatedAt"]).toBe(written["updatedAt"]);
  }, 60_000);

  test("CONTROL: the in-memory store does not survive its process", async () => {
    // Same probe binary, same key, no database. It round-trips inside one
    // process and carries nothing to the next one — which is exactly what the
    // durable store must be shown to do differently, and why the previous test
    // is evidence rather than tautology.
    const inProcess = await runProbe([
      "memory-secret-roundtrip",
      "--provider",
      PROVIDER,
      "--client",
      CLIENT_ID,
      "--secret",
      PLAINTEXT,
      ...probeArgs,
    ]);
    expect(inProcess["revealed"]).toBe(PLAINTEXT);

    const fresh = new InMemoryConsoleSecretStore(KEY);
    expect(await fresh.reveal(PROVIDER)).toBeNull();
  }, 60_000);

  test("a second pool over the same database sees the first pool's write", async () => {
    // The cheaper half of the same claim, kept because it isolates a different
    // failure: a store that cached in a module-level singleton would pass the
    // cross-process test (each process re-reads) but fail this one.
    const first = createConsolePool(dsn);
    const second = createConsolePool(dsn);
    try {
      await new PostgresConsoleSecretStore(first, KEY).put(PROVIDER, CLIENT_ID, PLAINTEXT);
      expect(await new PostgresConsoleSecretStore(second, KEY).reveal(PROVIDER)).toBe(PLAINTEXT);
    } finally {
      await first.end();
      await second.end();
    }
  });
});
