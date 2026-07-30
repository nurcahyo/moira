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
 * Automated accessibility gate — CONVENTIONS.md §3 ("automated a11y assertions
 * (axe) on every page-level route") and plan 08's "zero critical/serious
 * violations gates CI".
 *
 * The route list is discovered from `app/**` at collection time rather than
 * hard-coded, so a page added by a later wave is gated automatically. Today
 * that is just `/`; once plan 08 lands `/setup`, `/login`, `/dashboard`,
 * `/providers`, … each one grows its own test with no edit here.
 */

const routes: DiscoveredRoute[] = discoverPageRoutes(APP_DIR);

const AXE_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];
const BLOCKING_IMPACTS = new Set(["critical", "serious"]);

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

  for (const route of visitableRoutes(routes)) {
    test(`no critical or serious axe violations on ${route.pattern}`, async ({
      page,
    }, testInfo) => {
      const response = await page.goto(route.url, {
        waitUntil: "domcontentloaded",
      });
      expect(response!.status(), `${route.url} returned HTTP ${response!.status()}`).toBeLessThan(
        400,
      );

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
