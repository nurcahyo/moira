// The Better Auth options that DEFINE THE SCHEMA, and nothing else.
//
// ============================================================================
// WHY THIS FILE EXISTS INSTEAD OF `@better-auth/cli`
// ============================================================================
//
// Plan 09 §0.2 D2 says "Better Auth CLI migrations in `console/db/`". That
// cannot be done as written, and the reason is a version fact rather than a
// design preference:
//
//   better-auth        1.6.25   (pinned in package.json)
//   @better-auth/cli   1.4.21   (npm `latest`; `release-1.4` is 1.4.22,
//                                `beta` is 1.5.0-beta.13)
//
// There is no `@better-auth/cli` for the 1.6 line at all, and the 1.4 CLI
// carries its OWN `better-auth: 1.4.21` dependency — so `bunx @better-auth/cli
// generate` would emit the schema of a library two minor versions behind the
// one this console runs. That is precisely the drift a generated schema is
// supposed to eliminate.
//
// What the CLI's `generate`/`migrate` commands actually do is call
// `getMigrations()` from `better-auth/db/migration`. That entry point IS
// exported by the pinned 1.6.25 package, so `db/generate.ts` and `db/migrate.ts`
// call it directly. Same code path, correct version, no extra dependency.
//
// ============================================================================
// WHY THE OPTIONS HERE ARE NOT `createConsoleAuth`'s
// ============================================================================
//
// `createConsoleAuth` needs a resolved provider configuration — a client id, a
// client secret, a discovery URL — which come from Moira and the console's own
// secret store at runtime. A schema generator has neither and must not need
// them: the tables Better Auth creates depend only on the CORE options and the
// PLUGIN SET, never on any provider's credentials.
//
// The risk that follows is that this file and `lib/auth.ts` drift apart and the
// migration stops matching the running instance. That is not left to review:
// `tests/integration/console-db-migrations.test.ts` builds BOTH — the real
// instance from `createConsoleAuth` and the generator options below — runs
// `getMigrations()` on each against the same empty database, and fails if the
// table/field sets differ. Add a plugin to `lib/auth.ts` and forget this file
// and that test goes red before the missing table ever reaches a deployment.
import { betterAuth } from "better-auth";
import { genericOAuth } from "better-auth/plugins/generic-oauth";
import { jwt } from "better-auth/plugins/jwt";

/**
 * Placeholder provider credentials.
 *
 * Every one of these is a non-secret literal, and they are deliberately named
 * so that a reader who greps for a leaked client secret finds an obvious fake.
 * They exist only because `genericOAuth` requires the fields to be present; the
 * plugin performs no network call and reads no credential during schema
 * generation.
 */
const SCHEMA_ONLY_PLACEHOLDER = "schema-generation-placeholder-not-a-credential";

/** Matches `lib/env.ts`'s `AUTH_JWKS_PATH`; duplicated so this file imports no
 *  `server-only` module (it runs under plain `bun`, outside Next.js). */
const JWKS_PATH = "/.well-known/jwks.json";

/**
 * The options whose schema the committed migrations must satisfy.
 *
 * `database` is supplied by the caller: `getMigrations()` introspects the live
 * database to decide what is missing, so the same options object is used
 * against an empty database (to generate) and against a migrated one (to prove
 * nothing is pending).
 */
export function consoleAuthSchemaOptions(database: unknown): Parameters<typeof betterAuth>[0] {
  return {
    appName: "Moira Console",
    // Not read during schema generation; present because `betterAuth` validates
    // its options eagerly.
    baseURL: "https://console.invalid",
    basePath: "/api/auth",
    secret: "schema-generation-only-secret-at-least-32-chars",
    database: database as Parameters<typeof betterAuth>[0]["database"],

    // Mirrors `lib/auth.ts`. It matters for the schema: enabling the password
    // path would make `account.password` load-bearing rather than vestigial.
    emailAndPassword: { enabled: false },

    // Mirrors `lib/auth.ts`, and it IS the schema: this is the column
    // `db/migrations/0003_session_provider.sql` adds. `required: false` is what
    // makes it nullable, and the nullability is load-bearing — every session
    // live when that migration runs has no value and must REFUSE to mint rather
    // than default. `input: false` keeps it off every API surface; it does not
    // affect the DDL, and it is repeated here so the two option sets stay
    // literally identical rather than merely schema-equivalent.
    session: {
      additionalFields: {
        providerId: { type: "string", required: false, input: false },
      },
    },

    // Mirrors `lib/auth.ts`, and it DOES change the schema: `getAuthTables`
    // adds a `rateLimit` table only when storage is `"database"`. Omitting it
    // here would leave the migration short of a table the running instance
    // writes to on its very first request.
    rateLimit: { storage: "database" },

    plugins: [
      genericOAuth({
        config: [
          {
            providerId: "schema-generation-placeholder",
            clientId: SCHEMA_ONLY_PLACEHOLDER,
            clientSecret: SCHEMA_ONLY_PLACEHOLDER,
            discoveryUrl: "https://idp.invalid/.well-known/openid-configuration",
            pkce: true,
          },
        ],
      }),

      // The reason a durable database exists at all: this plugin's ES256 key
      // pair lives in the `jwks` table. With `memoryAdapter` it is regenerated
      // per process, and the JWKS document Moira fetched a minute ago stops
      // verifying newly minted tokens.
      jwt({
        jwks: {
          keyPairConfig: { alg: "ES256" },
          jwksPath: JWKS_PATH,
        },
        jwt: {
          issuer: "https://console.invalid",
          audience: "schema-generation-only",
        },
      }),
    ],
  };
}
