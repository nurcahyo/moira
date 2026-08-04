import { defineConfig, devices } from "@playwright/test";

import { AUTH_CONSOLE_ORIGIN, AUTH_STORAGE_STATE } from "./e2e/support/authenticated-fixture";
import { installConsoleE2eEnv } from "./e2e/support/console-env";
// Imported for its LOAD-TIME EFFECT as much as for the number: `fixed-ports.ts`
// runs `assertFixedPortsDisjoint()` at import, so two harnesses claiming one port
// fail here — at config load, before a build — instead of as an EADDRINUSE from
// whichever server lost the race five minutes in.
import { SCAFFOLD_CONSOLE_PORT } from "./e2e/support/fixed-ports";
import { SENTINEL_ENV } from "./e2e/support/secrets";
import {
  SETUP_FIXTURE_BASE_URL,
  SETUP_FIXTURE_CONSOLE_PORT,
} from "./e2e/support/setup-fixture";

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
 * Declared in `e2e/support/fixed-ports.ts` with every other fixed port in the
 * harness — see that file for why they are in one place. Override the whole run
 * with CONSOLE_E2E_PORT.
 */
const port = SCAFFOLD_CONSOLE_PORT;

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

/** The one spec that runs against the setup fixture rather than `baseURL`. */
const SETUP_FIXTURE_SPEC = "**/setup-wizard.e2e.ts";

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

  /**
   * ==========================================================================
   * TWO CONSOLES, AND WHICH SPEC RUNS AGAINST WHICH
   * ==========================================================================
   *
   * `chromium` is the scaffold: `https://moira.invalid`, `db.invalid:1`, no IdP.
   * The smoke, a11y, i18n-key, secret-leak and gate specs all want that, because
   * a suite that proves "nothing real is in reach" is stronger when nothing real
   * IS in reach.
   *
   * `authenticated` is the second stack (`e2e/support/authenticated-stack.ts`):
   * the same standalone artifact with a fixture Moira, a real mock IdP, the
   * console's own database, and a mock OpenAI-compatible endpoint. It is the
   * "authenticated Playwright project" whose absence `a11y.e2e.ts` and
   * `llm-settings.e2e.ts` both record as a REVERSAL CONDITION.
   *
   * The split is by `testMatch`/`testIgnore` rather than by folder so the two
   * environments can never be pointed at each other's specs by accident: an
   * authenticated spec run against `moira.invalid` would fail on the first click,
   * and a leak scan run against the authenticated stack would be scanning a
   * server whose secrets were never harvested as needles.
   */
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
      // The setup-wizard spec drives a DIFFERENT server (see `SETUP_FIXTURE_*`),
      // so it must not run against `baseURL`, on top of the two existing
      // authenticated specs that already have their own project below.
      testIgnore: [
        /authenticated-session\.e2e\.ts/,
        /llm-settings-authenticated\.e2e\.ts/,
        SETUP_FIXTURE_SPEC,
      ],
    },
    {
      // Signs in for real, through the browser, and saves the storage state.
      // Separate from the suite that uses it so a broken sign-in reports itself
      // as a broken sign-in rather than as six failing assertions about the LLM
      // screen.
      name: "authenticated-setup",
      testMatch: /authenticated-session\.e2e\.ts/,
      use: {
        ...devices["Desktop Chrome"],
        baseURL: AUTH_CONSOLE_ORIGIN,
        // The fixture certificate is self-signed and trusted by the console
        // process through NODE_EXTRA_CA_CERTS. A browser has no such store here.
        ignoreHTTPSErrors: true,
      },
    },
    {
      name: "authenticated",
      testMatch: /llm-settings-authenticated\.e2e\.ts/,
      dependencies: ["authenticated-setup"],
      use: {
        ...devices["Desktop Chrome"],
        baseURL: AUTH_CONSOLE_ORIGIN,
        ignoreHTTPSErrors: true,
        storageState: AUTH_STORAGE_STATE,
      },
    },
    /**
     * ========================================================================
     * THE SETUP WIZARD'S OWN CONSOLE (issue #71, AC4)
     * ========================================================================
     *
     * A second server, a second baseURL, and exactly one spec file. Everything
     * about why it exists is in `e2e/support/setup-fixture.ts`; what matters
     * here is what it does NOT do: it does not change the main `chromium`
     * project, whose console still has no system key and still points at
     * `https://moira.invalid`. Every existing assertion keeps testing the
     * server it was written against.
     *
     * `fullyParallel: false` plus `test.describe.serial` in the spec: the stub
     * Moira holds ONE deployment's state, and the spec walks that deployment
     * from fresh through provisioned to claimed. Two workers interleaving on it
     * would be two operators sharing one first-run.
     */
    {
      name: "setup-wizard",
      use: { ...devices["Desktop Chrome"], baseURL: SETUP_FIXTURE_BASE_URL },
      testMatch: SETUP_FIXTURE_SPEC,
      fullyParallel: false,
    },
  ],

  ...(externalBaseURL
    ? {}
    : {
        webServer: [
          {
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
              // ----------------------------------------------------------------
              // PINNED AGAINST A DEVELOPER'S `.env.local`
              // ----------------------------------------------------------------
              //
              // Next loads `.env.local` from the working directory in the
              // STANDALONE RUNTIME too, and `@next/env` only skips a variable
              // that is already defined in `process.env` (empty string included —
              // it tests `typeof`). Every other value this server needs is
              // already pinned, either here or by `installConsoleE2eEnv()`; these
              // two were not, so a developer who had run `scripts/dev-env.sh` got
              // a different gate from CI:
              //
              //   CONSOLE_ALLOW_INSECURE_URLS  refused outright in production —
              //     the server would not boot at all.
              //   MOIRA_SYSTEM_KEY             would OPEN the setup window on
              //     this server, so `/setup` would stop rendering the refusal
              //     panel that `a11y.e2e.ts` documents it as rendering, and would
              //     start reaching for `https://moira.invalid` on every request.
              //
              // Empty rather than absent for the key, because absent is what
              // `.env.local` would fill in.
              CONSOLE_ALLOW_INSECURE_URLS: "false",
              MOIRA_SYSTEM_KEY: "",
              // Server-only sentinel secrets for the secret-leak gate.
              // Never prefixed NEXT_PUBLIC_ — see e2e/support/secrets.ts.
              ...SENTINEL_ENV,
            },
          },
          {
            /**
             * The authenticated stack. It BUILDS NOTHING — the entry above owns
             * `bun run build`, and two Next builds writing one `.next`
             * concurrently is a corrupted output — so it waits for that server
             * to answer before spawning its own copy of the same artifact.
             *
             * `--preload` is load-bearing: the stack seeds the console's OAuth
             * client secret through the SHIPPED `PostgresConsoleSecretStore`,
             * which carries `import "server-only"`, and that package's `default`
             * export condition is a bare `throw`. Same neutralisation
             * `tests/support/durability-probe.ts` already uses for a plain `bun`
             * process.
             */
            command:
              "bun --preload ./tests/support/server-only-runtime-shim.ts " +
              "e2e/support/authenticated-stack.ts",
            url: `${AUTH_CONSOLE_ORIGIN}/login`,
            ignoreHTTPSErrors: true,
            reuseExistingServer: !isCI,
            // Includes waiting for the first entry's build.
            timeout: 300_000,
            stdout: "pipe" as const,
            stderr: "pipe" as const,
            // No `env`. Everything the console needs is composed by the stack
            // itself and handed to the child it spawns, and the ports come from
            // `e2e/support/fixed-ports.ts`, which the stack imports directly.
            // The `CONSOLE_E2E_AUTH_PORT` that used to be set here was read by
            // nothing — a value that has to agree with `AUTH_CONSOLE_ORIGIN` and
            // is passed through a second channel is a second place for the two
            // to disagree.
          },
          /**
           * ====================================================================
           * THE SETUP-WIZARD FIXTURE STACK
           * ====================================================================
           *
           * A stub Moira on TLS plus a SECOND copy of the standalone server, both
           * owned by one child process so Playwright can start and kill them as a
           * unit. See `e2e/support/setup-fixture.ts` for why each piece is shaped
           * the way it is, and `e2e/support/run-setup-fixture.ts` for the order.
           *
           * It carries no `command` of its own beyond that script and NO BUILD:
           * `webServer` entries are started in order (each plugin's `setup` is
           * awaited before the next), so the FIRST entry above has already
           * produced `.next/standalone` by the time this one runs. Building
           * twice would double the slowest step in the suite for no additional
           * evidence. It does not depend on the `authenticated` stack entry
           * either — the two run independently, both after the first build.
           *
           * The readiness URL is `/setup` rather than `/`, deliberately: `/` is
           * inside `(console)` and answers a redirect whatever the setup window
           * is doing, so it would report "ready" for a fixture whose Moira never
           * came up. `/setup` is the surface these tests are about.
           */
          {
            command: "bun e2e/support/run-setup-fixture.ts",
            url: `${SETUP_FIXTURE_BASE_URL}/setup`,
            reuseExistingServer: !isCI,
            timeout: 120_000,
            stdout: "pipe" as const,
            stderr: "pipe" as const,
            env: {
              PORT: String(SETUP_FIXTURE_CONSOLE_PORT),
            },
          },
        ],
      }),
});
