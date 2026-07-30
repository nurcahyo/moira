import { expect, test } from "@playwright/test";

import { attachLeakTap, scanBuildOutput } from "./support/leak-tap";
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
