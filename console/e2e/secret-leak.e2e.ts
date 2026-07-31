import { expect, test } from "@playwright/test";

import { attachLeakTap, scanBuildOutput } from "./support/leak-tap";
import { CONSOLE_E2E_ENV } from "./support/console-env";
import { NEXT_BUILD_DIR, APP_DIR } from "./support/paths";
import { discoverPageRoutes, visitableRoutes } from "./support/routes";
import {
  describeLeaks,
  forbiddenValues,
  scanForLeaks,
  SENTINEL_ENV,
  type Leak,
} from "./support/secrets";

/**
 * ============================================================================
 * SECRET-LEAK GATE — CONVENTIONS.md §3, §7.5 and §8 ("No secret-leak: verified
 * by test").
 * ============================================================================
 *
 * WHY THIS EXISTS
 * ---------------
 * Moira's load-bearing security invariant is that a system key, an admin key,
 * an OAuth client secret, the console's encryption key, or a decrypted provider
 * credential never crosses into the browser. Violating it does not break any
 * feature — the console keeps working perfectly while handing the key to every
 * visitor — so nothing but a test will ever notice. §6 rule 5 states the same
 * thing structurally: a secret must never be passed as a prop past the
 * page/server boundary, because organisms/molecules/atoms render client-side.
 *
 * WHAT WOULD MAKE IT FAIL
 * -----------------------
 *   - a server-only env value serialised into rendered HTML or RSC flight data
 *     (the classic: reading `process.env.X` in a component that turns out to be
 *     a client component, or passing it down as a prop);
 *   - a secret inlined into a `.next/static/**` chunk or captured in a sourcemap;
 *   - a secret echoed in any browser-observed response body;
 *   - a secret written to the browser console;
 *   - re-exporting a secret as `NEXT_PUBLIC_*` (name rule — fires even if the
 *     value is unknown to this test);
 *   - shipping PEM private-key material, e.g. publishing the Better Auth JWT
 *     signing key instead of the JWKS public half.
 *
 * HOW IT SURVIVES PLAN 08
 * -----------------------
 * No real secret exists in the scaffold, so `playwright.config.ts` seeds
 * high-entropy sentinels into the console server's environment (server-only,
 * never `NEXT_PUBLIC_*`). But the needle set is computed by `forbiddenValues()`,
 * which ALSO harvests every ambient env var whose name looks like a secret. The
 * moment plan 08 sets `MOIRA_SYSTEM_KEY` / `BETTER_AUTH_SECRET` /
 * `GOOGLE_CLIENT_SECRET` / `CONSOLE_SECRET_ENCRYPTION_KEY` for the e2e run,
 * those real values are gated automatically with no edit to this file. Routes
 * are discovered from `app/**`, so new pages are gated automatically too.
 *
 * The assertion is that the violation list is EMPTY — not that "this run's
 * sentinel happened to be absent" — so the gate keeps its teeth as the surface
 * grows. See e2e/support/secrets.ts for the full rationale.
 *
 * PROVEN TO FAIL: verified by temporarily rendering
 * `process.env.MOIRA_E2E_SENTINEL_SYSTEM_KEY` from a page, which produced
 * failures in both the browser-surface scan and the build-output scan; the leak
 * was then reverted. Re-run that experiment before trusting any change here.
 *
 * ============================================================================
 * WHAT PLAN 09 WAVE 1 ADDED, AND WHAT IT DELIBERATELY DID NOT
 * ============================================================================
 *
 * Durable storage is a new place a secret can leak, in two distinct shapes.
 *
 *   1. THE CONNECTION STRING. `CONSOLE_DATABASE_URL` carries the database
 *      password inline, and its name matches none of the SECRET/KEY/TOKEN
 *      patterns the ambient harvester keys on — so it would have been the one
 *      console credential this gate never looked for. `secrets.ts` now extracts
 *      the password half as a needle, and adds a value-independent pattern for
 *      any `postgres://user:pass@` string, which fires even on a DSN this run
 *      never knew (a driver error message, a stack trace, a config dump).
 *      Covered here, on every route and over the build output.
 *
 *   2. THE CIPHERTEXT AT REST. Not covered here, on purpose. This suite scans
 *      what the BROWSER sees, and `console_provider_secret` has no browser
 *      surface at all in this wave — no route reads it. Asserting over a table
 *      from Playwright would also mean importing `lib/console-secrets-postgres.ts`,
 *      whose `import "server-only"` throws outside a Next.js server build.
 *      That coverage lives in `tests/integration/console-secret-store-postgres.test.ts`
 *      ("nothing readable is written to the database"), which has a real
 *      PostgreSQL and reads back every column of every row.
 *
 * REVERSE THAT SPLIT the moment a route renders anything derived from the
 * secret store — a drift banner, a masked fingerprint, a "configured" badge.
 * At that point the value is on a page and belongs in the loop below.
 *
 * PROVEN TO FAIL (plan 09 Wave 1): `<p>{process.env.CONSOLE_DATABASE_URL}</p>`
 * was temporarily rendered from `app/page.tsx`; both the browser-surface scan
 * and the build-output scan failed, reporting "secret value from
 * CONSOLE_DATABASE_URL password (ambient server env)". The leak was reverted.
 *
 * ONE HARNESS PROPERTY THAT EXPERIMENT EXPOSED — worth knowing before trusting
 * a green run: with `E2E_SKIP_BUILD=1` the SAME injected leak passed. `/` is
 * statically prerendered, so a server-side `process.env` read is resolved at
 * BUILD time; reusing a `.next` produced without the e2e environment means the
 * value was never in the output to find. `E2E_SKIP_BUILD=1` is a local
 * iteration shortcut and must not be used for a gating run.
 */

const needles = forbiddenValues();
const routes = visitableRoutes(discoverPageRoutes(APP_DIR));

test.describe("secret leak", () => {
  test("the gate has needles and routes to scan", () => {
    // A gate with an empty needle set or an empty route set passes trivially.
    // Fail loudly instead of pretending to be green.
    expect(
      needles.length,
      "no forbidden values configured — the secret-leak gate would be vacuous",
    ).toBeGreaterThanOrEqual(Object.keys(SENTINEL_ENV).length);
    expect(
      routes.length,
      "no page-level routes discovered — the secret-leak gate would be vacuous",
    ).toBeGreaterThan(0);
  });

  test("no server-only secret is inlined in the built client bundle", () => {
    const result = scanBuildOutput(NEXT_BUILD_DIR, needles);

    expect(
      result.filesScanned,
      `no build output found under ${NEXT_BUILD_DIR} — run \`bun run build\` first; ` +
        "an unscanned bundle is an unverified bundle",
    ).toBeGreaterThan(0);

    expect(
      result.leaks,
      `secret material found in the build output:\n${describeLeaks(result.leaks)}`,
    ).toEqual([]);
  });

  test("the database password is one of the needles", () => {
    // The DSN's name matches none of the SECRET/KEY/TOKEN patterns, and the DSN
    // itself contains `/` so it can never be a needle on its own. If the
    // password extraction in `secrets.ts` regresses, every other assertion in
    // this file still passes while the console's newest credential goes
    // unchecked — so it is asserted directly.
    const password = new URL(CONSOLE_E2E_ENV["CONSOLE_DATABASE_URL"]!).password;
    expect(password.length, "the e2e DSN has no password to check for").toBeGreaterThan(12);
    expect(
      needles.map((needle) => needle.value),
      "the database password was not extracted from CONSOLE_DATABASE_URL, so no scan in " +
        "this file is looking for it",
    ).toContain(password);
  });

  test("no NEXT_PUBLIC_* variable is named like a secret", () => {
    // Next.js inlines every referenced NEXT_PUBLIC_* value into the client
    // bundle. A secret-shaped name is a leak waiting to happen even before the
    // value is used, so it is rejected on sight (CONVENTIONS §7.5).
    const offenders = Object.keys(process.env).filter(
      (name) =>
        name.startsWith("NEXT_PUBLIC_") && /(SECRET|KEY|TOKEN|PASSWORD|CREDENTIAL)/i.test(name),
    );
    expect(
      offenders,
      "these NEXT_PUBLIC_* variables carry secret-shaped names and are shipped to the browser",
    ).toEqual([]);
  });

  for (const route of routes) {
    test(`no secret reaches the browser on ${route.pattern}`, async ({ page }) => {
      const tap = attachLeakTap(page);

      const response = await page.goto(route.url, { waitUntil: "load" });
      expect(response!.status()).toBeLessThan(400);
      await page.waitForLoadState("networkidle");

      const leaks: Leak[] = [];

      // 1. Everything the browser received: HTML, RSC flight data, JS chunks,
      //    JSON, CSS — plus console output and uncaught errors.
      for (const blob of await tap.drain()) {
        leaks.push(...scanForLeaks(blob.where, blob.content, needles));
      }

      // 2. The live DOM, which also covers anything injected after hydration.
      leaks.push(
        ...scanForLeaks(`rendered DOM of ${route.pattern}`, await page.content(), needles),
      );
      leaks.push(
        ...scanForLeaks(
          `document.documentElement.outerHTML of ${route.pattern}`,
          await page.evaluate(() => document.documentElement.outerHTML),
          needles,
        ),
      );

      expect(
        leaks,
        `secret material observable from the browser on ${route.pattern}:\n${describeLeaks(leaks)}`,
      ).toEqual([]);
    });
  }
});
