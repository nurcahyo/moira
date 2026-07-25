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

const port = Number(process.env.CONSOLE_E2E_PORT ?? 3100);

/** Set CONSOLE_E2E_BASE_URL to test an already-running console (no webServer). */
const externalBaseURL = process.env.CONSOLE_E2E_BASE_URL;
const baseURL = externalBaseURL ?? `http://127.0.0.1:${port}`;

const isCI = process.env.CI === "true" || process.env.CI === "1";

/**
 * The suite runs against a *production* build by default, because that is the
 * artifact the Dockerfile ships and the only one whose client bundles are
 * representative for the secret-leak scan. `E2E_SKIP_BUILD=1` reuses an
 * existing `.next` for fast local iteration.
 */
const startCommand =
  process.env.E2E_SKIP_BUILD === "1" ? "bun run start" : "bun run build && bun run start";

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
