// The console's own configuration, for the e2e server.
//
// ============================================================================
// WHY THESE ARE SET ON `process.env` AND NOT ONLY ON `webServer.env`
// ============================================================================
//
// `lib/env.ts` validates eagerly and fails the boot when any of these is
// missing, so the e2e server needs them. `webServer.env` alone would supply
// them — but it would also put them ONLY in the server's environment, and
// `e2e/support/secrets.ts`'s `forbiddenValues()` harvests needles from the
// PLAYWRIGHT RUNNER's `process.env`. Set only on the server, they would never
// become needles, and the secret-leak gate would keep passing while ignoring the
// console's actual secrets.
//
// Assigning them here — at config load, with `??=` so a real environment always
// wins — puts them in the runner's environment, from which the server inherits
// them, AND makes `BETTER_AUTH_SECRET` and `CONSOLE_SECRET_ENCRYPTION_KEY`
// automatic needles by name (both match `secrets.ts`'s SECRET/KEY pattern).
// That is the forward-compatibility hook `secrets.ts` describes, actually
// connected up.
//
// Every value below is chosen to satisfy `isUsableNeedle`: at least 12
// characters, no whitespace, and no `/` or `\` (a base64 key containing `/`
// would be discarded as a needle and silently stop being checked).

/** Fixture configuration. Never a deployment value; every entry is public. */
export const CONSOLE_E2E_ENV: Readonly<Record<string, string>> = {
  // Unreachable on purpose. The scaffold's specs make no Moira call, and a
  // resolvable host would let a regression pass by talking to something real.
  MOIRA_API_URL: "https://moira.invalid",
  MOIRA_ADMIN_API_AUDIENCE: "moira-console-e2e-audience",
  // 32 bytes, base64, and deliberately free of `+` and `/`.
  CONSOLE_SECRET_ENCRYPTION_KEY: "1wEFCqo384H0xewVWQUmKeDYFHvzNZnaokjN4v2M1GQ=",
  BETTER_AUTH_SECRET: "moira-console-e2e-better-auth-secret-2f7a41c9e6",
  // Required because the e2e server runs with NODE_ENV=production, where
  // `lib/env.ts` refuses to boot without it — the console must not reach the
  // ephemeral storage path by omission in anything production-shaped.
  //
  // Port 1 and the `.invalid` TLD make it unreachable BY CONSTRUCTION: env
  // validation is shape-only and `pg` connects lazily, so nothing in this suite
  // touches a database, and a regression that started opening one at boot fails
  // loudly instead of quietly talking to a developer's local PostgreSQL.
  //
  // The password is the point. It is high-entropy and `/`-free so that
  // `forbiddenValues()` picks it out of the DSN and every route scan, plus the
  // build-output scan, checks for it. Without a DSN here, the console's newest
  // credential would be the one value the leak gate never looked for.
  CONSOLE_DATABASE_URL:
    "postgres://moira-console-e2e:e2e-db-password-8d41cb07f2ae9536@db.invalid:1/console_auth_e2e",
};

/**
 * Install the fixture configuration into the current process.
 *
 * `??=`, so a value supplied by the surrounding environment always wins: a run
 * against a real fixture Moira must not be overridden by these.
 *
 * `CONSOLE_PUBLIC_ORIGIN` is NOT here — it depends on the port the config
 * chooses, so `playwright.config.ts` sets it alongside `PORT`.
 */
export function installConsoleE2eEnv(): void {
  for (const [name, value] of Object.entries(CONSOLE_E2E_ENV)) {
    process.env[name] ??= value;
  }
}
