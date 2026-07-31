// @server-only
//
// FINDING F17 — the console must never advertise a key it cannot sign with.
//
// ============================================================================
// THE DEFECT THIS EXISTS TO MAKE UNREACHABLE
// ============================================================================
//
// `BETTER_AUTH_SECRET` signs session cookies **and** encrypts the jwt plugin's
// ES256 private key in the `jwks` table (better-auth 1.6.25
// `plugins/jwt/utils.ts::createJwk`; encryption is on unless
// `jwks.disablePrivateKeyEncryption` is set, and this console does not set it —
// see `JWKS_OPTIONS` in `lib/auth.ts`).
//
// Rotate that secret against a DURABLE database and the two halves of the key
// pair part company, in the worst available shape:
//
//   * `getJwks` reads the **plaintext `publicKey` column** and serves it
//     verbatim. The JWKS document is byte-identical, still 200s, and Moira's
//     cached copy stays valid;
//   * `signJWT` must `symmetricDecrypt` the private half and raises
//     "Failed to decrypt private key";
//   * and it does **not** regenerate — `sign.mjs` mints a new pair only when
//     there is NO key or the key has EXPIRED, and an undecryptable row is
//     neither.
//
// So the console keeps publishing a key it can no longer sign for. Sign-in
// still works, the JWKS endpoint still looks healthy, and every Moira-bound
// token the console mints is rejected with nothing pointing at the cause. With
// the memory adapter this was self-healing (a restart regenerated the pair);
// durable storage converted it into a silent, persistent outage.
//
// Since wave 4B the blast radius is N issuers, not one: `admin_identities` is
// keyed on `(issuer, subject)` and every provider now mints a different `iss`,
// but they all share ONE key pair and ONE JWKS URL. One undecryptable row takes
// every provider's admin path down together.
//
// ============================================================================
// THE MECHANISM, AND WHY IT IS AT THIS SEAM AND NOT AT STARTUP
// ============================================================================
//
// `plugins/jwt/adapter.mjs` routes BOTH key reads through one optional
// override:
//
//     getAllKeys:   if (options?.adapter?.getJwks) return options.adapter.getJwks(ctx)
//     getLatestKey: if (options?.adapter?.getJwks) return (…getJwks(ctx))?.sort(…)[0]
//
// `getAllKeys` is what the JWKS endpoint publishes; `getLatestKey` is what
// `signJWT` signs with. Installing ONE function here therefore makes
// "published ⊆ signable" true **by construction** rather than by two checks
// that have to agree. There is no arrangement in which the document and the
// signer read different sets, because they read the same call.
//
// A boot-time probe was considered and rejected as the mechanism (it is welcome
// as an *addition*, not as the guarantee):
//
//   * it is a point-in-time sample. The row can stop being decryptable after
//     boot — a second replica writing under a different secret, a restore from
//     a backup taken under an older one — and a startup check has already run;
//   * Next.js has no hook that runs after the pool exists and before the first
//     route handler, so it would have to live in the request path anyway;
//   * a boot-time database probe turns a transient database blip into a console
//     that refuses to start, which is a new outage bought with the fix for an
//     old one.
//
// ============================================================================
// WHY IT REFUSES RATHER THAN REGENERATES
// ============================================================================
//
// Returning `[]` here would restore the memory adapter's self-healing: the
// plugin mints a fresh pair when the set is empty, on both paths. It was
// rejected. See the decision record in `docs/console-storage.md`; the short
// version is that regeneration cannot tell "the operator rotated deliberately"
// from "the operator supplied the WRONG secret", and in the second case it
// destroys a recoverable state by minting a new console identity. Refusing
// preserves the old key material intact, so putting the right secret back is a
// complete recovery with zero JWKS churn and no window in which Moira's cache
// disagrees with what the console signs.
import "server-only";

import { APIError } from "better-auth/api";
import { symmetricDecrypt } from "better-auth/crypto";
import type { Jwk, JwtOptions } from "better-auth/plugins/jwt";

/* -------------------------------------------------------------------------- */
/* The types, taken from the option they implement                            */
/* -------------------------------------------------------------------------- */

type JwksAdapterOption = NonNullable<JwtOptions["adapter"]>;
type GetJwks = NonNullable<JwksAdapterOption["getJwks"]>;

/**
 * The slice of Better Auth's endpoint context the check reads.
 *
 * Derived from the option's own parameter type rather than imported from
 * `@better-auth/core`, so this module does not depend on a deep subpath of a
 * transitive package to name a shape it only reads two fields of.
 */
export type JwksKeyContext = Parameters<GetJwks>[0]["context"];

/**
 * The body `code` on the refusal, and the string an operator greps for.
 *
 * Deliberately not the library's "Failed to decrypt private key": that message
 * is only ever produced on the SIGNING path, which is the half of F17 that was
 * already loud. This one is what the JWKS endpoint says.
 */
export const JWKS_UNSIGNABLE_CODE = "JWKS_KEY_UNSIGNABLE";

/**
 * What a caller of the JWKS or token endpoint is told.
 *
 * Terse on purpose. The endpoint is anonymous, so the body says only that the
 * console is refusing to publish; the remedy goes to the process log, where the
 * operator is.
 */
const JWKS_UNSIGNABLE_MESSAGE =
  "The console cannot sign with its stored JWKS key pair and is refusing to publish it.";

/** The full operator remedy. Logged, never returned over the wire. */
export function unsignableJwksLogLine(unusableKeyIds: readonly string[]): string {
  return (
    `[F17] every row in the console's \`jwks\` table (${unusableKeyIds.length}: ` +
    `${unusableKeyIds.join(", ")}) failed to decrypt under the CURRENT BETTER_AUTH_SECRET. ` +
    "That secret encrypts the ES256 private key as well as signing session cookies, so it was " +
    "almost certainly rotated or supplied incorrectly. The console is refusing to publish a " +
    "JWKS it cannot sign for — a healthy-looking JWKS with a dead signer is the outage this " +
    "refusal exists to prevent. Either put the previous BETTER_AUTH_SECRET back (a complete " +
    "recovery: the key material is intact and Moira's cached JWKS stays valid), or accept a new " +
    "key pair by deleting the rows — see docs/console-storage.md, section " +
    "'BETTER_AUTH_SECRET'."
  );
}

/** Is `error` this module's refusal? */
export function isUnsignableJwksError(error: unknown): boolean {
  return (
    error instanceof Error &&
    error.name === "APIError" &&
    (error as { body?: { code?: unknown } }).body?.code === JWKS_UNSIGNABLE_CODE
  );
}

/* -------------------------------------------------------------------------- */
/* The predicate                                                              */
/* -------------------------------------------------------------------------- */

/**
 * Can this process sign with `row`?
 *
 * The two operations are exactly the two `plugins/jwt/sign.mjs` performs before
 * it reaches `importJWK`, in the same order, through the same `symmetricDecrypt`
 * and the same `ctx.context.secretConfig`. That is deliberate and it is the
 * whole reason this is trustworthy: a check that *re-derived* the answer could
 * disagree with the signer, and disagreement in the permissive direction is the
 * defect itself.
 *
 * `privateKeyEncryptionEnabled` mirrors `sign.mjs`'s own ternary rather than
 * being inferred from the stored value's shape. Inferring it would be wrong in
 * the one case that matters: a bare (unencrypted) JWK sitting in a table
 * belonging to a console that has encryption ON is a row the signer WILL try to
 * decrypt and WILL fail on, and a shape-sniffing check would call it usable.
 */
async function canSignWith(
  row: Jwk,
  secretConfig: JwksKeyContext["secretConfig"],
  privateKeyEncryptionEnabled: boolean,
): Promise<boolean> {
  try {
    const privateWebKey = privateKeyEncryptionEnabled
      ? await symmetricDecrypt({ key: secretConfig, data: JSON.parse(row.privateKey) as string })
      : row.privateKey;
    // `sign.mjs` hands the result to `importJWK(JSON.parse(privateWebKey), alg)`.
    // A value that does not parse is not a key it can sign with either.
    JSON.parse(privateWebKey);
    return true;
  } catch {
    return false;
  }
}

export interface JwksPartition {
  /** Rows whose private half decrypts under the current secret. */
  readonly usable: Jwk[];
  /** The `id` (== `kid`) of every row that does not. */
  readonly unusableKeyIds: string[];
}

/**
 * Split the stored `jwks` rows into what this process can sign with and what it
 * cannot.
 *
 * An EMPTY table is not a partition failure: it is a virgin database, and the
 * plugin's own "no keys, mint one" path is the correct behaviour there. Only a
 * table with rows, none of them usable, is F17.
 */
export async function partitionJwks(
  context: JwksKeyContext,
  privateKeyEncryptionEnabled: boolean,
): Promise<JwksPartition> {
  const rows = (await context.adapter.findMany<Jwk>({ model: "jwks" })) ?? [];
  const usable: Jwk[] = [];
  const unusableKeyIds: string[] = [];
  for (const row of rows) {
    if (await canSignWith(row, context.secretConfig, privateKeyEncryptionEnabled)) usable.push(row);
    else unusableKeyIds.push(row.id);
  }
  return { usable, unusableKeyIds };
}

/* -------------------------------------------------------------------------- */
/* The installed override                                                     */
/* -------------------------------------------------------------------------- */

/**
 * The jwt plugin's `adapter.getJwks`, filtered so that nothing this process
 * cannot sign with is ever published or selected.
 *
 * Pass the SAME `jwks` options object the plugin is configured with, so
 * `disablePrivateKeyEncryption` cannot drift between the signer's behaviour and
 * this check's model of it.
 */
export function signableJwksAdapter(jwksOptions: JwtOptions["jwks"]): JwksAdapterOption {
  const privateKeyEncryptionEnabled = !jwksOptions?.disablePrivateKeyEncryption;

  return {
    getJwks: async (ctx) => {
      const { usable, unusableKeyIds } = await partitionJwks(
        ctx.context,
        privateKeyEncryptionEnabled,
      );

      if (usable.length === 0 && unusableKeyIds.length > 0) {
        // Logged before throwing: the wire body is deliberately terse, so this
        // is the only place the remedy is stated, and the JWKS endpoint is
        // anonymous.
        ctx.context.logger.error(unsignableJwksLogLine(unusableKeyIds));
        // An `APIError`, not a bare throw. better-call renders it as a 503 with
        // `{ message, code }`; a bare `Error` becomes an opaque 500 with the
        // body dropped, and "the console is broken" is a materially worse thing
        // to tell an operator than "the console is refusing to serve keys".
        throw new APIError("SERVICE_UNAVAILABLE", {
          message: JWKS_UNSIGNABLE_MESSAGE,
          code: JWKS_UNSIGNABLE_CODE,
        });
      }

      return usable;
    },
  };
}
