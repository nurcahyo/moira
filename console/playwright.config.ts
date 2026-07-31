import { defineConfig, devices } from "@playwright/test";

import { installConsoleE2eEnv } from "./e2e/support/console-env";
import { SENTINEL_ENV } from "./e2e/support/secrets";

// Must run before `forbiddenValues()` reads `process.env` — see console-env.ts.
installConsoleE2eEnv();

/**
 * Playwright configuration for the Moira console.
 *
 * Scope note: this is the workspace scaffold's e2e harness only. Plan 08 owns
 * the feature specs (setup wizard, mock-OIDC sign-in, config round-trip,
 * authorization denial) and will add a second `webServer` entry for the
 * test-fixture Moira instance and the local mock OIDC provider. Nothing here
 * talks to Moira or to any auth provider.
 *
 * Spec files use the `*.e2e.ts` suffix rather than `*.spec.ts` on purpose:
 * CONVENTIONS §5 makes Bun the *unit* test runner, and `bun test` claims
 * `*.spec.*` as well as `*.test.*`. A `.spec.ts` file under `console/` would be
 * picked up by both runners and fail under Bun. `testMatch` below keeps the two
 * suites disjoint without needing a bunfig root override.
 */

/**
 * Deliberately not 3000 (a dev `next dev` would clash) and not 3100 (commonly
 * taken by local Docker port mappings). Override with CONSOLE_E2E_PORT.
 */
const port = Number(process.env.CONSOLE_E2E_PORT ?? 3210);

/** Set CONSOLE_E2E_BASE_URL to test an already-running console (no webServer). */
const externalBaseURL = process.env.CONSOLE_E2E_BASE_URL;
const baseURL = externalBaseURL ?? `http://127.0.0.1:${port}`;

const isCI = process.env.CI === "true" || process.env.CI === "1";

/**
 * The suite runs against a *production* build, served by the **standalone**
 * server — i.e. exactly the artifact `Dockerfile` ships. `next start` is not
 * used: Next.js warns that it "does not work with output: standalone", and a
 * secret-leak gate that scans a differently-assembled server is a weaker gate
 * than one that scans the real one.
 *
 * Assembling standalone means copying the two directories Next deliberately
 * leaves out of `.next/standalone` (the client assets in `.next/static` and
 * `public/`), which is the same copy the Dockerfile's runtime stage performs.
 * The `rm -rf` avoids the `static/static` nesting you get from re-copying onto
 * an existing directory.
 *
 * `E2E_SKIP_BUILD=1` reuses an existing `.next` for fast local iteration, and is
 * REFUSED UNDER CI — see `assertGatingRunBuildsItsOwnOutput()` below.
 */
const assembleAndStart = [
  "rm -rf .next/standalone/.next/static .next/standalone/public",
  "cp -R .next/static .next/standalone/.next/static",
  "(cp -R public .next/standalone/public || true)",
  "node .next/standalone/server.js",
].join(" && ");

/**
 * ============================================================================
 * WHY `E2E_SKIP_BUILD=1` IS A HARD ERROR UNDER CI
 * ============================================================================
 *
 * `e2e/secret-leak.e2e.ts` scans two surfaces: what the BROWSER receives, and
 * the BUILD OUTPUT under `.next`. The second one is only meaningful if the build
 * it scans was produced with the sentinel environment in scope — a value that
 * was never in the environment at build time cannot be inlined into the output,
 * so the scan finds nothing and reports success.
 *
 * `.github/workflows/ci.yml` used to set this flag on the Playwright step to
 * reuse the `bun run build` from an earlier step — a step whose job env contains
 * only `CONSOLE_TEST_DATABASE_URL`. The gating run therefore scanned a
 * sentinel-free bundle on every CI run.
 *
 * REPRODUCED, plan 09 wave 3, before the fix: a statically prerendered `/`
 * rendering `{process.env.MOIRA_E2E_SENTINEL_SYSTEM_KEY}`, built from a shell
 * with no sentinels (exactly the CI job env) and then run with
 * `E2E_SKIP_BUILD=1` (exactly the CI step) produced **13 passed, exit 0**. The
 * injected leak was invisible to every assertion in the suite, including the
 * browser-surface scan — because a static page resolves `process.env` at BUILD
 * time, so the running server never had the value either.
 *
 * The same leak under a full build fails loudly. So the flag is refused here,
 * rather than merely removed from the workflow: removing it from one file is
 * undone by one edit to that file, and the failure it reintroduces is a silently
 * green gate.
 *
 * It remains available locally, where a fast iteration loop is worth more than a
 * gate nobody is currently relying on.
 */
function assertGatingRunBuildsItsOwnOutput(): void {
  if (isCI && process.env.E2E_SKIP_BUILD === "1") {
    throw new Error(
      "E2E_SKIP_BUILD=1 is refused under CI. The secret-leak gate's build-output scan is " +
        "vacuous against a `.next` produced without the sentinel environment: a value that was " +
        "never in the build's env cannot be found in its output. Drop the flag so " +
        "`webServer.command` builds with the e2e environment in scope.",
    );
  }
}

assertGatingRunBuildsItsOwnOutput();

const startCommand =
  process.env.E2E_SKIP_BUILD === "1" ? assembleAndStart : `bun run build && ${assembleAndStart}`;

export default defineConfig({
  testDir: "./e2e",
  testMatch: "**/*.e2e.ts",
  outputDir: "./test-results",

  fullyParallel: true,
  forbidOnly: isCI,
  retries: isCI ? 2 : 0,
  // Serial in CI for stable timings against a single Next server; unset locally
  // so Playwright picks a worker count from the machine.
  ...(isCI ? { workers: 1 } : {}),
  timeout: 30_000,
  expect: { timeout: 10_000 },

  reporter: isCI
    ? [["list"], ["github"], ["html", { open: "never" }]]
    : [["list"], ["html", { open: "never" }]],

  use: {
    baseURL,
    trace: "on-first-retry",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },

  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],

  ...(externalBaseURL
    ? {}
    : {
        webServer: {
          command: startCommand,
          url: baseURL,
          reuseExistingServer: !isCI,
          timeout: 300_000,
          stdout: "pipe" as const,
          stderr: "pipe" as const,
          env: {
            // `next start` honours PORT.
            PORT: String(port),
            NODE_ENV: "production",
            // `lib/env.ts` validates eagerly and refuses to boot without
            // these. The rest come from `installConsoleE2eEnv()` above, which
            // the server inherits; only the origin is decided here.
            //
            // NOTE the deliberate mismatch: the server listens on plain http on
            // loopback, but declares an https PUBLIC origin. That is not a
            // fudge — it is what a real deployment looks like. The console runs
            // behind a TLS-terminating ingress (`charts/moira-console`'s
            // Ingress forces an https redirect), so the origin it advertises as
            // its issuer and JWKS host is never the address it binds.
            //
            // The alternative — declaring `http://127.0.0.1:PORT` and setting
            // `CONSOLE_ALLOW_INSECURE_URLS=true` — CANNOT work here, because
            // `NODE_ENV` is `production` two lines above and `lib/env.ts` makes
            // that flag a hard failure in production, exactly as Moira's
            // `Settings::validate` does for `auth.jwks.allow_insecure_dev_urls`.
            // Trying it fails the boot with `ConsoleConfigError`.
            CONSOLE_PUBLIC_ORIGIN: "https://console.e2e.invalid",
            // Server-only sentinel secrets for the secret-leak gate.
            // Never prefixed NEXT_PUBLIC_ — see e2e/support/secrets.ts.
            ...SENTINEL_ENV,
          },
        },
      }),
});
