import { expect, test } from "@playwright/test";

import { CONSOLE_CATALOG } from "../lib/i18n/catalog.en";
import { CONSOLE_MESSAGE_KEYS } from "../lib/i18n/keys";
import { DYNAMIC_ROUTE_FIXTURES, INVITE_ROUTE_FIXTURE_TOKEN } from "./support/routes";

/**
 * ============================================================================
 * THE INVITATION PAGE, ON A TOKEN THAT NAMES NOTHING
 * ============================================================================
 *
 * This is the ONE invitation e2e this harness can honestly drive today, and it
 * is worth saying why the others are absent rather than shipping green shells of
 * them.
 *
 * `MOIRA_API_URL` is `https://moira.invalid` and there is no authenticated
 * Playwright storage state (see `a11y.e2e.ts`'s header and decision W5-D7). So
 * `invite-redeem`, `invite-domain-policy` and `ownership-transfer` — every one
 * of which needs a live Moira AND a session — could only be written here as
 * tests that navigate somewhere, get redirected or refused, and assert nothing
 * about the behaviour they are named for. That is precisely the failure this
 * project keeps paying for: a suite that reads as coverage.
 *
 * What IS drivable is the unauthenticated half, and it carries three properties
 * that genuinely matter:
 *
 *   1. an unusable invitation renders as a PAGE with a keyed explanation, not as
 *      a 404 — which is what keeps the a11y walker able to audit this route at
 *      all, and is the right answer for a human holding an expired link;
 *   2. the console never echoes the token back into the document, even though it
 *      is in the URL;
 *   3. the refusal is Moira's own message, rendered through `t()`, rather than a
 *      console-invented string.
 *
 * On a deployment with a reachable Moira, (3) is `invite_not_found`; here it is
 * the transport failure's key. Both are the same code path — the page has no
 * branch that invents copy — and the assertion is written against the shape
 * rather than the particular refusal so it stays true either way.
 */

const FIXTURE_URL = DYNAMIC_ROUTE_FIXTURES["/invite/[token]"]!;

test.describe("an invitation that cannot be used", () => {
  test("renders as a page with a keyed explanation, not a 404", async ({ page }) => {
    const response = await page.goto(FIXTURE_URL, { waitUntil: "domcontentloaded" });

    expect(
      response!.status(),
      "an unreadable invitation is a condition the holder needs explained, not a missing " +
        "document — and a 404 here would take the a11y gate red for an ordinary expired link",
    ).toBeLessThan(400);

    const body = (await page.locator("body").innerText()).trim();
    expect(body).toContain(CONSOLE_CATALOG[CONSOLE_MESSAGE_KEYS.invite_unusable_heading].message);
  });

  test("the token is never rendered into visible copy", async ({ page }) => {
    // ========================================================================
    // WHAT THIS ASSERTS, AND THE STRONGER THING IT DELIBERATELY DOES NOT
    // ========================================================================
    //
    // The obvious assertion is `page.content()` does not contain the token. It
    // FAILS, and it fails for a correct page: Next.js serialises the concrete
    // dynamic segment into the router state inside the RSC payload, so the token
    // is in the document exactly because it is in the URL the browser already
    // holds. Asserting otherwise would be asserting that `/invite/[token]` is
    // not a dynamic route.
    //
    // Verified rather than assumed — the `page.content()` form was written
    // first, run, and failed on this page with no token rendered anywhere in it.
    //
    // What IS a real property, and what this asserts: the token does not appear
    // in VISIBLE TEXT. A screenshot, a support paste, or a copied error report
    // carries what a human can see; the address bar is a channel they already
    // know about, and an error banner quoting the token is one they do not.
    //
    // The other half of the containment — that it reaches no third party through
    // a `Referer` — is `smoke.e2e.ts`'s no-foreign-origin assertion on this same
    // route.
    await page.goto(FIXTURE_URL, { waitUntil: "load" });

    const visible = await page.locator("body").innerText();
    expect(
      visible.includes(INVITE_ROUTE_FIXTURE_TOKEN),
      "the invitation page rendered the token as copy an operator can read and paste",
    ).toBe(false);
  });

  test("the accept control is not offered to a visitor who cannot use it", async ({ page }) => {
    // No session, and no usable invitation. A rendered "Accept invitation"
    // button here is a control whose only possible outcome is a refusal.
    await page.goto(FIXTURE_URL, { waitUntil: "load" });
    await expect(
      page.getByRole("button", {
        name: CONSOLE_CATALOG[CONSOLE_MESSAGE_KEYS.invite_accept].message,
      }),
    ).toHaveCount(0);
  });
});
