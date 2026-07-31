// One operation, one process, one exit.
//
// ============================================================================
// WHY A CHILD PROCESS AND NOT A SECOND OBJECT
// ============================================================================
//
// "Durable" means "survives the process". Constructing a second store against
// the same pool inside one test proves the SQL round-trips; it does not prove
// that nothing load-bearing was being held in a module-level variable, a
// closure, or a `Map` that happens to still be there. Both of the properties
// this wave exists to establish are process-lifetime properties:
//
//   * the sealed OAuth client secret must outlive a pod restart;
//   * the jwt plugin's ES256 key pair must be the SAME key pair after a restart,
//     because Moira caches the JWKS document and a new key means every freshly
//     minted token fails verification against the cached copy.
//
// So the tests spawn this file, let it exit, and spawn it again. The state
// that survives is the state in PostgreSQL and nothing else.
//
// It is invoked through `bun --preload tests/support/server-only-runtime-shim.ts`
// because every module it imports carries `import "server-only"`.
//
// Output is one JSON object on stdout. Diagnostics go to stderr so a parse
// failure in the parent is a real failure and not a stray log line.
import { createLocalJWKSet, decodeProtectedHeader, jwtVerify, type JSONWebKeySet } from "jose";

import { createConsoleAuth } from "../../lib/auth";
import { createConsolePool } from "../../lib/console-db";
import { InMemoryConsoleSecretStore } from "../../lib/console-secrets";
import { PostgresConsoleSecretStore } from "../../lib/console-secrets-postgres";
import { readConsoleEnv } from "../../lib/env";
import type { ResolvedAuthConfig } from "../../lib/auth-config";
import { reserveConsolePort, startConsoleServer } from "./console-server";
import { trustFixtureCa } from "./fixture-tls";

function flag(name: string): string | undefined {
  const index = process.argv.indexOf(`--${name}`);
  return index === -1 ? undefined : process.argv[index + 1];
}

function requiredFlag(name: string): string {
  const value = flag(name);
  if (value === undefined || value === "") throw new Error(`--${name} is required`);
  return value;
}

/**
 * The probe's own environment.
 *
 * Built through the real `readConsoleEnv` rather than by hand, so a probe cannot
 * pass while the shipped validation would have refused the same values.
 * `BETTER_AUTH_SECRET` is supplied by the caller and must be IDENTICAL across
 * two runs of the JWKS probe: it is also the key that encrypts the stored ES256
 * private key, so changing it makes a durable `jwks` row unreadable and Better
 * Auth mints a new pair. That is a real property worth being able to demonstrate,
 * and one of the tests does.
 */
const PROBE_AUDIENCE = "moira-admin-probe";

function probeEnv(databaseUrl: string | undefined, origin = "https://console.invalid") {
  return readConsoleEnv({
    NODE_ENV: "test",
    MOIRA_API_URL: "https://moira.invalid",
    CONSOLE_PUBLIC_ORIGIN: origin,
    MOIRA_ADMIN_API_AUDIENCE: PROBE_AUDIENCE,
    BETTER_AUTH_SECRET: requiredFlag("auth-secret"),
    CONSOLE_SECRET_ENCRYPTION_KEY: requiredFlag("encryption-key"),
    ...(databaseUrl === undefined ? {} : { CONSOLE_DATABASE_URL: databaseUrl }),
  });
}

/** A provider configuration good enough to construct Better Auth. No network. */
function probeAuthConfig(): ResolvedAuthConfig {
  return {
    providerId: "moira-console-idp",
    method: "generic_oidc",
    moiraProviderId: "probe-provider",
    moiraProviderVersion: 1,
    issuer: "https://idp.invalid",
    discoveryUrl: "https://idp.invalid/.well-known/openid-configuration",
    authorizationUrl: null,
    tokenUrl: null,
    userInfoUrl: null,
    clientId: "probe-client-id",
    clientSecret: "probe-client-secret-not-a-real-credential",
    scopes: ["openid", "email", "profile"],
    allowedEmailDomains: ["example.com"],
    trustedJwtIssuerId: "00000000-0000-0000-0000-000000000000",
    // The probe never mints, so this is only ever the string the plugin would
    // stamp. It is the console's own issuer, never the IdP's — see `lib/auth.ts`.
    consoleIssuer: "https://console.invalid",
  };
}

/**
 * A Better Auth instance over the console's database, or over the memory
 * adapter when no `--dsn` is given.
 *
 * The memory case passes NO `database` at all rather than passing
 * `createConsoleMemoryDatabase()`: that helper returns the bare table object,
 * and Better Auth's `createKyselyAdapter` does not recognise it — it needs
 * `memoryAdapter(...)` wrapped around it, which is exactly what
 * `createConsoleAuth`'s own default does. Handing the raw object over produces
 * `BetterAuthError: Failed to initialize database adapter`.
 */
function buildProbeAuth(dsn: string | undefined): {
  auth: ReturnType<typeof createConsoleAuth>;
  close: () => Promise<void>;
} {
  const env = probeEnv(dsn);
  const pool = dsn === undefined ? null : createConsolePool(dsn);
  const auth = createConsoleAuth({
    env,
    configs: [probeAuthConfig()],
    ...(pool === null ? {} : { database: pool }),
  });
  return { auth, close: async () => void (pool === null ? undefined : await pool.end()) };
}

async function main(): Promise<void> {
  const command = process.argv[2];

  switch (command) {
    /* ------------------------------------------------------------------ */
    /* Secret durability                                                   */
    /* ------------------------------------------------------------------ */
    case "secret-put": {
      const dsn = requiredFlag("dsn");
      const env = probeEnv(dsn);
      const pool = createConsolePool(dsn);
      try {
        const store = new PostgresConsoleSecretStore(pool, env.secretEncryptionKey);
        await store.put(requiredFlag("provider"), requiredFlag("client"), requiredFlag("secret"));
        const sealed = await store.read(requiredFlag("provider"));
        process.stdout.write(
          JSON.stringify({ ok: true, updatedAt: sealed?.updatedAt ?? null }) + "\n",
        );
      } finally {
        await pool.end();
      }
      return;
    }

    case "secret-reveal": {
      const dsn = requiredFlag("dsn");
      const env = probeEnv(dsn);
      const pool = createConsolePool(dsn);
      try {
        const store = new PostgresConsoleSecretStore(pool, env.secretEncryptionKey);
        const provider = requiredFlag("provider");
        const revealed = await store.reveal(provider);
        const sealed = await store.read(provider);
        process.stdout.write(
          JSON.stringify({
            ok: true,
            revealed,
            clientId: sealed?.clientId ?? null,
            updatedAt: sealed?.updatedAt ?? null,
            newestUpdatedAt: await store.newestUpdatedAt(),
          }) + "\n",
        );
      } finally {
        await pool.end();
      }
      return;
    }

    /* ------------------------------------------------------------------ */
    /* The in-memory control                                               */
    /* ------------------------------------------------------------------ */
    //
    // A test that only shows "the durable store round-trips" would pass just as
    // happily against a store that never restarts. These two cases make the
    // suite able to demonstrate the DIFFERENCE, which is the actual claim.
    case "memory-secret-roundtrip": {
      const env = probeEnv(undefined);
      const store = new InMemoryConsoleSecretStore(env.secretEncryptionKey);
      await store.put(requiredFlag("provider"), requiredFlag("client"), requiredFlag("secret"));
      process.stdout.write(
        JSON.stringify({ ok: true, revealed: await store.reveal(requiredFlag("provider")) }) + "\n",
      );
      return;
    }

    /* ------------------------------------------------------------------ */
    /* JWKS stability                                                      */
    /* ------------------------------------------------------------------ */
    case "jwks": {
      const dsn = flag("dsn");
      const { auth, close } = buildProbeAuth(dsn);
      try {
        // Better Auth generates the key pair lazily, on the first JWKS read, and
        // persists it in the `jwks` table. So this call is both "read the
        // document" and "create it if this is a virgin database".
        const jwks = (await auth.api.getJwks()) as { keys: Record<string, unknown>[] };
        process.stdout.write(
          JSON.stringify({
            ok: true,
            mode: dsn === undefined ? "memory" : "durable",
            // The whole public half. Comparing `kid` alone would pass if the kid
            // were derived from something stable while the key itself changed.
            keys: jwks.keys,
          }) + "\n",
        );
      } finally {
        await close();
      }
      return;
    }

    /**
     * Can this process still SIGN with the stored key pair?
     *
     * Separate from `jwks` because the two answers can disagree, and the way
     * they disagree is the sharp edge: `getJwks` serves the `publicKey` column
     * verbatim, while signing has to `symmetricDecrypt` the `privateKey` column
     * under `BETTER_AUTH_SECRET`. Rotate that secret and the console keeps
     * publishing a JWKS it can no longer sign for.
     *
     * The failure is reported rather than thrown, because "signing failed" is
     * the outcome under test in one of the cases.
     */
    case "sign": {
      const dsn = flag("dsn");
      const { auth, close } = buildProbeAuth(dsn);
      try {
        let token: string | null = null;
        let error: string | null = null;
        try {
          const result = (await auth.api.signJWT({
            body: { payload: { sub: "durability-probe" } },
          })) as unknown;
          token = typeof result === "string" ? result : JSON.stringify(result);
        } catch (signError) {
          error = (signError as Error).message;
        }
        process.stdout.write(JSON.stringify({ ok: true, token, error }) + "\n");
      } finally {
        await close();
      }
      return;
    }

    /* ------------------------------------------------------------------ */
    /* F17 — is the PUBLISHED document one this process can SIGN for?      */
    /* ------------------------------------------------------------------ */
    /**
     * The joint question, asked of the shipped stack over a real socket.
     *
     * `jwks` and `sign` above ask the two halves separately, which is what made
     * F17 visible in the first place. Separate answers cannot express the
     * property that matters, because the defect IS the disagreement: a JWKS
     * that 200s and a signer that is dead are each individually unremarkable.
     *
     * So this command runs BOTH in one process against one database and reports
     * whether the console can produce a token that verifies against the very
     * document it just served. Everything is the shipped path:
     *
     *   * `startConsoleServer` builds the instance with `createConsoleAuth` and
     *     routes through `app/api/auth/[...all]/route.ts` — not `auth.handler`,
     *     so the route module's own behaviour is included;
     *   * the JWKS document arrives over a real TLS socket via `fetch`, which is
     *     Moira's own path (`createRemoteJWKSet` against `jwks_url`), not an
     *     in-process object;
     *   * the token is verified with `jose` against THAT document, by `kid`,
     *     the way Moira verifies it.
     *
     * Nothing here re-implements the check the mechanism performs. A guard that
     * calls the predicate it is guarding proves the predicate compiles.
     */
    case "attest": {
      const dsn = flag("dsn");
      const port = reserveConsolePort();
      const origin = `https://localhost:${port}`;
      const env = probeEnv(dsn, origin);
      const pool = dsn === undefined ? null : createConsolePool(dsn);
      trustFixtureCa(origin);
      const server = startConsoleServer(
        {
          env,
          configs: [probeAuthConfig()],
          ...(pool === null ? {} : { database: pool }),
        },
        port,
      );

      try {
        // ---- 1. what the console PUBLISHES, over the wire ----------------
        let jwksStatus = 0;
        let jwksBody: unknown = null;
        let jwksError: string | null = null;
        try {
          const response = await fetch(server.jwksUrl);
          jwksStatus = response.status;
          jwksBody = (await response.json()) as unknown;
        } catch (error) {
          jwksError = (error as Error).message;
        }
        const publishedKeys =
          jwksStatus === 200 && jwksBody !== null && typeof jwksBody === "object"
            ? ((jwksBody as { keys?: unknown }).keys as Record<string, unknown>[] | undefined) ?? []
            : [];

        // ---- 2. what the console can SIGN --------------------------------
        let token: string | null = null;
        let signError: string | null = null;
        try {
          const result = (await server.auth.api.signJWT({
            body: { payload: { sub: "f17-attestation" } },
          })) as { token?: string } | string;
          token = typeof result === "string" ? result : (result.token ?? JSON.stringify(result));
        } catch (error) {
          signError = (error as Error).message;
        }

        // ---- 3. does (2) verify against (1)? -----------------------------
        //
        // Moira's verification, reproduced: resolve the signing key by `kid`
        // out of the fetched document, check the signature, check `iss` and
        // `aud`. "A token was minted" and "a JWKS was served" are both true in
        // the broken arrangement; only this step is not.
        let verifiedKid: string | null = null;
        let verifyError: string | null = null;
        if (token !== null && publishedKeys.length > 0) {
          try {
            const keySet = createLocalJWKSet({ keys: publishedKeys } as JSONWebKeySet);
            await jwtVerify(token, keySet, {
              issuer: env.bffIssuerUrl,
              audience: PROBE_AUDIENCE,
            });
            verifiedKid = decodeProtectedHeader(token).kid ?? null;
          } catch (error) {
            verifyError = (error as Error).message;
          }
        }

        process.stdout.write(
          JSON.stringify({
            ok: true,
            mode: dsn === undefined ? "memory" : "durable",
            jwksStatus,
            jwksBody,
            jwksError,
            publishedKids: publishedKeys.map((key) => key["kid"]),
            token,
            signError,
            verifiedKid,
            verifyError,
          }) + "\n",
        );
      } finally {
        server.stop();
        if (pool !== null) await pool.end();
      }
      return;
    }

    default:
      throw new Error(`unknown probe command: ${String(command)}`);
  }
}

try {
  await main();
} catch (error) {
  process.stderr.write(`${(error as Error).stack ?? String(error)}\n`);
  process.stdout.write(JSON.stringify({ ok: false, error: (error as Error).message }) + "\n");
  process.exitCode = 1;
}
