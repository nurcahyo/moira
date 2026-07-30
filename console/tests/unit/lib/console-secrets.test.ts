// The D7 envelope: what it protects against, asserted rather than asserted-to.

import { describe, expect, test } from "bun:test";

import {
  classifySecretDrift,
  InMemoryConsoleSecretStore,
  openClientSecret,
  sealClientSecret,
  SECRET_ENVELOPE_VERSION,
  SecretEnvelopeError,
  type SealedClientSecret,
} from "@/lib/console-secrets";

const KEY = Buffer.alloc(32, 0xa7);
const OTHER_KEY = Buffer.alloc(32, 0x5c);
const PROVIDER = "11111111-1111-4111-8111-111111111111";
const CLIENT_ID = "console.apps.example.test";
const SECRET = "s3cr3t-oauth-client-secret";

describe("the envelope round-trips", () => {
  test("seal then open returns the plaintext", () => {
    const sealed = sealClientSecret(KEY, PROVIDER, CLIENT_ID, SECRET);
    expect(openClientSecret(KEY, PROVIDER, sealed)).toBe(SECRET);
  });

  test("the plaintext appears nowhere in the persisted record", () => {
    const sealed = sealClientSecret(KEY, PROVIDER, CLIENT_ID, SECRET);
    // Everything about the record is persistable; the whole point is that
    // nothing in it is the secret.
    const serialised = JSON.stringify(sealed);
    expect(serialised).not.toContain(SECRET);
    expect(sealed.clientId).toBe(CLIENT_ID);
    expect(sealed.version).toBe(SECRET_ENVELOPE_VERSION);
  });

  test("two seals of the same secret differ", () => {
    // A deterministic ciphertext would tell an observer with read access to the
    // store when two providers share a secret, and when a rotation was a no-op.
    const a = sealClientSecret(KEY, PROVIDER, CLIENT_ID, SECRET);
    const b = sealClientSecret(KEY, PROVIDER, CLIENT_ID, SECRET);
    expect(a.ciphertext).not.toBe(b.ciphertext);
    expect(a.iv).not.toBe(b.iv);
  });
});

describe("the envelope refuses what it should refuse", () => {
  test("a different content-encryption key cannot open it", () => {
    const sealed = sealClientSecret(KEY, PROVIDER, CLIENT_ID, SECRET);
    expect(() => openClientSecret(OTHER_KEY, PROVIDER, sealed)).toThrow(SecretEnvelopeError);
  });

  test("a tampered ciphertext is rejected, not silently truncated", () => {
    const sealed = sealClientSecret(KEY, PROVIDER, CLIENT_ID, SECRET);
    const bytes = Buffer.from(sealed.ciphertext, "base64url");
    bytes[0] = (bytes[0] ?? 0) ^ 0xff;
    const tampered: SealedClientSecret = { ...sealed, ciphertext: bytes.toString("base64url") };
    expect(() => openClientSecret(KEY, PROVIDER, tampered)).toThrow(SecretEnvelopeError);
  });

  test("moving the record to another provider breaks the AAD binding", () => {
    // The scenario: someone copies a row, or a restore lands a secret under the
    // wrong provider id. It must fail closed, not decrypt into the wrong OAuth
    // client.
    const sealed = sealClientSecret(KEY, PROVIDER, CLIENT_ID, SECRET);
    expect(() => openClientSecret(KEY, "22222222-2222-4222-8222-222222222222", sealed)).toThrow(
      SecretEnvelopeError,
    );
  });

  test("editing the stored client_id breaks the AAD binding", () => {
    const sealed = sealClientSecret(KEY, PROVIDER, CLIENT_ID, SECRET);
    const edited: SealedClientSecret = { ...sealed, clientId: "attacker.apps.example.test" };
    expect(() => openClientSecret(KEY, PROVIDER, edited)).toThrow(SecretEnvelopeError);
  });

  test("an unknown envelope version is refused rather than guessed at", () => {
    const sealed = sealClientSecret(KEY, PROVIDER, CLIENT_ID, SECRET);
    expect(() =>
      openClientSecret(KEY, PROVIDER, { ...sealed, version: SECRET_ENVELOPE_VERSION + 1 }),
    ).toThrow(SecretEnvelopeError);
  });

  test("a short key is refused, not padded", () => {
    expect(() => sealClientSecret(Buffer.alloc(16, 1), PROVIDER, CLIENT_ID, SECRET)).toThrow(
      SecretEnvelopeError,
    );
  });

  test("an empty secret is refused", () => {
    // Sealing "" would produce a record that opens successfully and then fails
    // the code exchange with `invalid_client`, which reads as drift.
    expect(() => sealClientSecret(KEY, PROVIDER, CLIENT_ID, "")).toThrow(SecretEnvelopeError);
  });

  test("the failure message never names the cause", () => {
    // Distinguishing "wrong key" from "tampered" from "wrong client_id" is an
    // oracle for anyone with write access to the store.
    const sealed = sealClientSecret(KEY, PROVIDER, CLIENT_ID, SECRET);
    let wrongKey = "";
    let wrongProvider = "";
    try {
      openClientSecret(OTHER_KEY, PROVIDER, sealed);
    } catch (error) {
      wrongKey = (error as Error).message;
    }
    try {
      openClientSecret(KEY, "33333333-3333-4333-8333-333333333333", sealed);
    } catch (error) {
      wrongProvider = (error as Error).message;
    }
    expect(wrongKey).toBe(wrongProvider);
    expect(wrongKey).not.toContain(SECRET);
  });
});

describe("the store", () => {
  test("reveal returns the plaintext, read never does", async () => {
    const store = new InMemoryConsoleSecretStore(KEY);
    await store.put(PROVIDER, CLIENT_ID, SECRET);
    expect(await store.reveal(PROVIDER)).toBe(SECRET);
    const sealed = await store.read(PROVIDER);
    expect(JSON.stringify(sealed)).not.toContain(SECRET);
  });

  test("an unknown provider reveals null rather than throwing", async () => {
    // The D7 drift condition is a normal state, not an exception: Moira can
    // hold a provider the console has no secret for.
    const store = new InMemoryConsoleSecretStore(KEY);
    expect(await store.reveal("nope")).toBeNull();
    expect(await store.read("nope")).toBeNull();
  });

  test("newestUpdatedAt tracks the most recent write, and feeds the cache key", async () => {
    let now = new Date("2026-01-01T00:00:00.000Z");
    const store = new InMemoryConsoleSecretStore(KEY, () => now);
    expect(await store.newestUpdatedAt()).toBeNull();

    await store.put(PROVIDER, CLIENT_ID, SECRET);
    expect(await store.newestUpdatedAt()).toBe("2026-01-01T00:00:00.000Z");

    now = new Date("2026-06-01T00:00:00.000Z");
    await store.put("other", CLIENT_ID, SECRET);
    expect(await store.newestUpdatedAt()).toBe("2026-06-01T00:00:00.000Z");
  });

  test("remove forgets the secret", async () => {
    const store = new InMemoryConsoleSecretStore(KEY);
    await store.put(PROVIDER, CLIENT_ID, SECRET);
    await store.remove(PROVIDER);
    expect(await store.reveal(PROVIDER)).toBeNull();
  });

  test("a re-put replaces rather than appends", async () => {
    const store = new InMemoryConsoleSecretStore(KEY);
    await store.put(PROVIDER, CLIENT_ID, SECRET);
    await store.put(PROVIDER, CLIENT_ID, "rotated-secret");
    expect(await store.reveal(PROVIDER)).toBe("rotated-secret");
  });
});

describe("drift classification — the whole cost of D7, named", () => {
  const sealed = sealClientSecret(KEY, PROVIDER, CLIENT_ID, SECRET);

  test("matching client ids are in sync", () => {
    expect(classifySecretDrift(CLIENT_ID, sealed)).toBe("in_sync");
  });

  test("no console secret for a Moira provider", () => {
    expect(classifySecretDrift(CLIENT_ID, null)).toBe("console_secret_missing");
  });

  test("the console's secret belongs to a different client id", () => {
    expect(classifySecretDrift("rotated.apps.example.test", sealed)).toBe("client_id_mismatch");
  });

  test("Moira's row carries no client id at all", () => {
    expect(classifySecretDrift(null, sealed)).toBe("moira_client_id_missing");
    expect(classifySecretDrift(undefined, sealed)).toBe("moira_client_id_missing");
    expect(classifySecretDrift("", sealed)).toBe("moira_client_id_missing");
  });

  test("a client id of a different length does not throw", () => {
    // `timingSafeEqual` throws on a length mismatch; the length check has to
    // come first or a rotation to a longer id crashes the sign-in page.
    expect(() => classifySecretDrift("much-longer-client-id-value", sealed)).not.toThrow();
    expect(classifySecretDrift("much-longer-client-id-value", sealed)).toBe("client_id_mismatch");
  });
});
