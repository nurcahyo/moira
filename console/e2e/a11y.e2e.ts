import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";

import { APP_DIR } from "./support/paths";
import {
  discoverPageRoutes,
  uncoveredRoutes,
  visitableRoutes,
  type DiscoveredRoute,
} from "./support/routes";

/**
 * ============================================================================
 * AUTOMATED ACCESSIBILITY GATE — and what it could NOT see until wave 5
 * ============================================================================
 *
 * CONVENTIONS.md §3 requires axe on every page-level route, and plan 08 required
 * "zero critical/serious violations gates CI". The route list is discovered from
 * `app/**` at collection time rather than hard-coded, so a page added by a later
 * wave is gated automatically.
 *
 * ----------------------------------------------------------------------------
 * BLOCKER W5-B6: THIS GATE WAS AUDITING `/login` FOR EVERY GATED ROUTE
 * ----------------------------------------------------------------------------
 *
 * There is no authenticated Playwright storage state anywhere in `console/e2e/`.
 * The `(console)` layout redirects an unauthenticated visitor to `/login`, and
 * `page.goto` FOLLOWS redirects — so `no critical or serious axe violations on /`
 * was navigating to `/`, landing on `/login`, and auditing that. It passed. It
 * had been passing since `/` moved inside the route group.
 *
 * The e2e environment makes this certain rather than merely likely:
 * `MOIRA_API_URL` is `https://moira.invalid`, `CONSOLE_DATABASE_URL` points at
 * `db.invalid:1`, and `smoke.e2e.ts` positively asserts the root route contacts
 * no external origin. `consoleRuntime()` therefore always fails and the gate
 * always redirects.
 *
 * ----------------------------------------------------------------------------
 * WHAT WAVE 5 DID ABOUT IT (decision W5-D7, as implemented)
 * ----------------------------------------------------------------------------
 *
 * The audited URL is now ASSERTED rather than assumed:
 *
 *   PUBLIC route  -> navigate, assert the final pathname IS the route's own,
 *                    assert `status < 400`, and run axe. If a public route ever
 *                    starts redirecting, this fails instead of silently auditing
 *                    somewhere else.
 *   GATED route   -> navigate, and assert it lands on `/login`. axe is NOT run,
 *                    and the route is NOT counted as audited.
 *
 * and the set of unaudited routes is pinned in BOTH directions against
 * `ROUTES_NOT_AUDITED_PENDING_AUTHENTICATED_E2E` below. That is the difference
 * between a silent gap and a declared one: a new gated route cannot join the
 * unaudited set without an edit here, and a route cannot leave it without one
 * either.
 *
 * §0.8.4 step 24 asked for the bare assertion plus a red suite ("expect `/` and
 * `/admins` to fail it until an authenticated e2e path exists. Record that
 * failure honestly"). A permanently red merge gate is not a gate — it blocks
 * every subsequent change for a reason unrelated to that change, and the
 * pressure to weaken it is then constant. The declared-exemption form keeps the
 * same information visible, fails on exactly the same drift, and can be merged.
 *
 * REVERSAL CONDITION: when an authenticated Playwright project exists (which
 * needs a mock IdP inside the e2e environment — `tests/support/mock-idp.ts`
 * serves the unit and integration suites, not this one), delete the entries from
 * the list below and KEEP the URL assertion. The assertion is what will prove
 * the authenticated run is doing anything.
 */

const routes: DiscoveredRoute[] = discoverPageRoutes(APP_DIR);

const AXE_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];
const BLOCKING_IMPACTS = new Set(["critical", "serious"]);

/** Where the `(console)` layout sends an unauthenticated visitor. */
const SIGN_IN_PATH = "/login";

/**
 * Routes this suite does NOT audit, and why.
 *
 * Every entry is a route inside `(console)`, which redirects to `/login` with no
 * authenticated storage state. Listed explicitly so the gap is a reviewed fact
 * rather than a silence: the test below asserts this list equals the set of
 * gated routes EXACTLY, so adding a gated screen without touching this file
 * fails, and so does leaving an entry here after the route stops being gated.
 */
const ROUTES_NOT_AUDITED_PENDING_AUTHENTICATED_E2E: readonly string[] = ["/", "/admins"];

/** The pathname a route's fixture URL resolves to. */
function expectedPathname(route: DiscoveredRoute): string {
  return new URL(route.url!, "http://fixture.invalid").pathname;
}

test.describe("accessibility", () => {
  test("route discovery found at least one page-level route", () => {
    // Without this, a broken discovery would silently reduce the a11y gate to
    // zero tests and still report a green run.
    expect(
      routes.map((r) => r.pattern),
      "no page-level routes discovered under app/ — the a11y gate would be vacuous",
    ).not.toEqual([]);
  });

  test("every page-level route is covered (dynamic routes need a fixture URL)", () => {
    const uncovered = uncoveredRoutes(routes);
    expect(
      uncovered.map((r) => r.pattern),
      "dynamic route(s) have no entry in DYNAMIC_ROUTE_FIXTURES, so they are not being " +
        "a11y-tested. Add a concrete fixture URL in e2e/support/routes.ts.",
    ).toEqual([]);
  });

  test("the unaudited set is exactly the declared one, in both directions", () => {
    // The assertion that keeps W5-B6 from returning as a silence. A gated route
    // added without an edit here fails on the left; a stale entry fails on the
    // right.
    const gated = routes
      .filter((route) => route.gated)
      .map((route) => route.pattern)
      .sort();
    expect(
      gated,
      "these routes are inside (console) and therefore cannot be audited without an " +
        "authenticated Playwright project. Add them to " +
        "ROUTES_NOT_AUDITED_PENDING_AUTHENTICATED_E2E, or remove entries that are no longer " +
        "gated — do NOT point the walker somewhere convenient.",
    ).toEqual([...ROUTES_NOT_AUDITED_PENDING_AUTHENTICATED_E2E].sort());
  });

  test("something is actually audited, and it is the routes this wave added", () => {
    // A floor, for the same reason every walker in this repo has one: if the
    // gated set grew to cover everything, every axe assertion below would vanish
    // and the suite would still be green.
    const audited = visitableRoutes(routes)
      .filter((route) => !route.gated)
      .map((route) => route.pattern);
    expect(audited.length, `audited routes: ${audited.join(", ")}`).toBeGreaterThanOrEqual(2);
    expect(audited).toContain("/login");
    expect(
      audited,
      "the public invitation page is the one new surface an unauthenticated human reaches, so " +
        "it is the one this gate must genuinely audit",
    ).toContain("/invite/[token]");
  });

  for (const route of visitableRoutes(routes)) {
    if (route.gated) {
      test(`${route.pattern} is gated and redirects to ${SIGN_IN_PATH} (NOT audited)`, async ({
        page,
      }) => {
        // Asserted rather than skipped. The old suite ran axe here and reported
        // the result as if it described this page; it described `/login`.
        await page.goto(route.url, { waitUntil: "domcontentloaded" });
        expect(
          new URL(page.url()).pathname,
          `${route.pattern} did not redirect to ${SIGN_IN_PATH}. If an authenticated e2e path ` +
            "now exists, remove it from ROUTES_NOT_AUDITED_PENDING_AUTHENTICATED_E2E and let " +
            "the axe branch below cover it.",
        ).toBe(SIGN_IN_PATH);
      });
      continue;
    }

    test(`no critical or serious axe violations on ${route.pattern}`, async ({
      page,
    }, testInfo) => {
      const response = await page.goto(route.url, {
        waitUntil: "domcontentloaded",
      });
      expect(response!.status(), `${route.url} returned HTTP ${response!.status()}`).toBeLessThan(
        400,
      );

      // W5-D7. Without this, a public route that starts redirecting is audited
      // at its destination and reported under its own name — which is exactly
      // what `/` did for two waves.
      expect(
        new URL(page.url()).pathname,
        `asked for ${route.url} and ended up at ${page.url()} — the audit below would be ` +
          "describing a different page from the one it is named for",
      ).toBe(expectedPathname(route));

      const results = await new AxeBuilder({ page }).withTags(AXE_TAGS).analyze();

      await testInfo.attach(`axe-${route.pattern.replace(/\W+/g, "_")}.json`, {
        body: JSON.stringify(results.violations, null, 2),
        contentType: "application/json",
      });

      const blocking = results.violations.filter((v) => BLOCKING_IMPACTS.has(v.impact ?? ""));

      const report = blocking
        .map(
          (v) =>
            `  [${v.impact}] ${v.id}: ${v.help}\n` +
            v.nodes.map((n) => `      ${n.target.join(" ")}`).join("\n") +
            `\n      ${v.helpUrl}`,
        )
        .join("\n");

      expect(
        blocking.map((v) => v.id),
        `critical/serious accessibility violations on ${route.pattern}:\n${report}`,
      ).toEqual([]);
    });
  }
});
