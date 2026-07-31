import { expect, test } from "@playwright/test";

import { ALL_CONSOLE_MESSAGE_KEYS } from "../lib/i18n/keys";
import { APP_DIR } from "./support/paths";
import { discoverPageRoutes, visitableRoutes } from "./support/routes";

/**
 * ============================================================================
 * NO OPERATOR EVER SEES A BARE MESSAGE KEY
 * ============================================================================
 *
 * `t(key, args, fallback)` resolves catalog → server message → THE KEY ITSELF.
 * That last resort is deliberate — a bare key is something an operator can quote
 * in a bug report, which beats an empty string — but it is a last resort, and
 * every time it fires on a shipped page it means a key with no catalog entry
 * reached a render.
 *
 * `tests/unit/lib/i18n-catalog-coverage.test.ts` catches that for CONSOLE keys,
 * by comparing the emitted set to the catalogue in both directions. It cannot
 * catch it for MOIRA keys: `t(notice.message_key, …)` and `t(state.messageKey, …)`
 * pass arbitrary server-supplied strings straight through, and the mirror in
 * `lib/moira-keys.ts` is hand-maintained and imported by no application module —
 * only by two tests. That gap is recorded as an escalation in plan 09 §0.8.5.
 *
 * This suite closes the rendered half of it: whatever key any page resolves,
 * from either namespace, it must not appear as VISIBLE TEXT.
 *
 * Visible text specifically, not the HTML: keys legitimately appear in the RSC
 * payload as component props, and asserting over `page.content()` would fail on
 * correct code.
 *
 * ============================================================================
 * WHAT IT DOES NOT COVER, AND WHY THAT IS NOT HIDDEN
 * ============================================================================
 *
 * It walks the routes an UNAUTHENTICATED visitor can reach, because that is the
 * set this harness can reach — see `a11y.e2e.ts`'s header. `/admins` renders
 * eleven Moira error codes and three notices and none of them is exercised here.
 * The floor below asserts the walk found real routes, so this suite cannot
 * silently shrink to nothing; it cannot assert coverage it does not have.
 */

const routes = visitableRoutes(discoverPageRoutes(APP_DIR)).filter((route) => !route.gated);

/** `moira.error.x` / `console.page.y` — either namespace, in visible copy. */
const BARE_KEY = /\b(?:moira|console)\.[a-z][a-z0-9_]*\.[a-z][a-z0-9_]*\b/;

test.describe("message keys never reach the screen", () => {
  test("the walk found routes to check", () => {
    // Without this the loop below is empty and the suite is green by having
    // nothing to do.
    expect(
      routes.map((route) => route.pattern),
      "no ungated routes discovered — this suite would assert nothing",
    ).not.toEqual([]);
    expect(routes.length).toBeGreaterThanOrEqual(2);
  });

  test("the detector matches a real key and not ordinary prose", () => {
    // A positive control, for the same reason every scanner in this repo has
    // one: a regex that has stopped matching reports zero violations.
    expect(BARE_KEY.test("console.error.moira_unreachable")).toBe(true);
    expect(BARE_KEY.test(ALL_CONSOLE_MESSAGE_KEYS[0]!)).toBe(true);
    expect(BARE_KEY.test("Sign in to this deployment. Then continue.")).toBe(false);
    expect(BARE_KEY.test("Expires 2026-08-01T00:00:00Z.")).toBe(false);
  });

  for (const route of routes) {
    test(`no bare message key is rendered on ${route.pattern}`, async ({ page }) => {
      await page.goto(route.url, { waitUntil: "load" });
      const text = await page.locator("body").innerText();
      const match = BARE_KEY.exec(text);
      expect(
        match?.[0] ?? null,
        `${route.pattern} rendered a raw message key. Either the key has no entry in ` +
          "lib/i18n/catalog.en.ts, or a Moira key reached t() with no server `message` to fall " +
          "back to.",
      ).toBeNull();
    });
  }
});
