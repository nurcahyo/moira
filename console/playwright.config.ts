import { defineConfig, devices } from "@playwright/test";

import { SENTINEL_ENV } from "./e2e/support/secrets";

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
 * `E2E_SKIP_BUILD=1` reuses an existing `.next` for fast local iteration.
 */
const assembleAndStart = [
  "rm -rf .next/standalone/.next/static .next/standalone/public",
  "cp -R .next/static .next/standalone/.next/static",
  "(cp -R public .next/standalone/public || true)",
  "node .next/standalone/server.js",
].join(" && ");

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
            // Server-only sentinel secrets for the secret-leak gate.
            // Never prefixed NEXT_PUBLIC_ — see e2e/support/secrets.ts.
            ...SENTINEL_ENV,
          },
        },
      }),
});
