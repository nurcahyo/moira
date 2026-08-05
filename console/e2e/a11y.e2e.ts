import { expect, test } from "@playwright/test";

import { auditPage } from "./support/axe";
import { APP_DIR } from "./support/paths";
import {
  discoverPageRoutes,
  uncoveredRoutes,
  visitableRoutes,
  type DiscoveredRoute,
} from "./support/routes";

/**
 * ============================================================================
 * WHAT THIS FILE AUDITS ON `/setup`, AND WHERE THE REST OF IT IS AUDITED
 * ============================================================================
 *
 * `/setup` is public, so the walker below visits it and axe runs — but THIS
 * server has no `MOIRA_SYSTEM_KEY` and points at `https://moira.invalid`, so
 * `withSetupWindow` answers `404 setup_unavailable` and `SetupWizard` renders
 * its refusal panel. That panel is a real state of a real deployment (the
 * operator removed the bootstrap key after finishing, which is what they are
 * told to do), so auditing it here is correct — but for two waves it was ALL
 * that was audited, and the line's name did not say so.
 *
 * The wizard's own steps — welcome, the auth-settings form, the sign-in
 * surface, done — are audited by `setup-wizard.e2e.ts`, against the setup
 * fixture console (`e2e/support/setup-fixture.ts`): the same standalone
 * artifact, given a bootstrap key and a stub Moira on loopback, so the window
 * actually opens and the steps actually render.
 *
 * REMAINING GAP, declared rather than implied: the `claim` SUB-SURFACE of
 * `SignInClaimStep` (`stage === "claim"`) is not reached by any e2e. It is
 * gated on a Better Auth session, which needs a completed OAuth round trip
 * against a mock IdP inside the e2e environment — issue #72's harness. Until
 * then it is covered by `tests/unit/modules/setup/SignInClaimStep.test.tsx`,
 * and `setup-wizard.e2e.ts` asserts positively that the fixture stops at
 * `sign_in`, so the gap cannot quietly become something else.
 *
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
 *
 * ----------------------------------------------------------------------------
 * UPDATE (issue #75): THAT PROJECT NOW EXISTS, AND THIS FILE STILL CANNOT USE IT
 * ----------------------------------------------------------------------------
 *
 * `e2e/support/authenticated-stack.ts` stands up a second console — a fixture
 * Moira, a mock IdP and the console's own database — and the `authenticated`
 * project drives it with a real session. `/settings/llm` IS axe-audited there,
 * signed in, in `e2e/llm-settings-authenticated.e2e.ts`.
 *
 * It stays on the list below anyway, and that is not an oversight. This walker
 * runs in the `chromium` project, against the SCAFFOLD server, where
 * `MOIRA_API_URL` is `https://moira.invalid` and every gated route still
 * redirects. The list's meaning is unchanged — "gated routes this walker cannot
 * audit" — and the bidirectional assertion below is what keeps that honest. What
 * changed is that `/settings/llm` is no longer UNAUDITED; it is audited
 * elsewhere, by a suite that could see it.
 *
 * `/` and `/admins` have no authenticated coverage yet. Moving them is a
 * separate change: each needs its own spec in the authenticated project, and
 * adding them to that project's `testMatch` without one would audit nothing
 * while looking like it had.
 */

const routes: DiscoveredRoute[] = discoverPageRoutes(APP_DIR);

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
const ROUTES_NOT_AUDITED_PENDING_AUTHENTICATED_E2E: readonly string[] = [
  "/",
  "/admins",
  // Issue #74. Inside `(console)`, so the walker below asserts it redirects to
  // `/login` and does NOT audit it. Listed rather than silently uncovered: the
  // bidirectional assertion means adding a gated screen without touching this
  // file fails, which is the only thing keeping the gap a reviewed fact.
  //
  // NOT a gap any more, though it is still on this list: issue #75 audits this
  // route WITH a session, in `e2e/llm-settings-authenticated.e2e.ts`, against
  // the second stack. See the UPDATE in this file's header for why the entry
  // stays.
  "/settings/llm",
];

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

      const audit = await auditPage(page, testInfo, route.pattern);

      expect(
        audit.ids,
        `critical/serious accessibility violations on ${route.pattern}:\n${audit.report}`,
      ).toEqual([]);
    });
  }
});
