import { expect, test } from "@playwright/test";

import { DYNAMIC_ROUTE_FIXTURES } from "./support/routes";

/**
 * Smoke: the console boots and serves its root route.
 *
 * Deliberately independent of Moira and of authentication — this must stay
 * green on a bare workspace with no Moira instance, no OAuth provider and no
 * session, so a red run here always means "the console itself is broken",
 * never "a dependency was unavailable".
 *
 * Assertions are behavioural rather than copy-specific so that plan 08 can
 * replace the placeholder page without rewriting the smoke test.
 */
test.describe("console smoke", () => {
  test("root route responds and renders visible content", async ({ page }) => {
    const pageErrors: string[] = [];
    page.on("pageerror", (error) => pageErrors.push(error.message));

    const response = await page.goto("/", { waitUntil: "domcontentloaded" });

    expect(response, "navigation returned no response").not.toBeNull();
    expect(response!.status(), `root route returned HTTP ${response!.status()}`).toBeLessThan(400);

    // Something was actually painted, not just a 200 with an empty document.
    const bodyText = (await page.locator("body").innerText()).trim();
    expect(bodyText.length, "document body rendered no visible text").toBeGreaterThan(0);

    // Baseline document requirements that the a11y gate also depends on.
    await expect(page.locator("html")).toHaveAttribute("lang", /\S/);
    expect((await page.title()).trim().length).toBeGreaterThan(0);

    expect(pageErrors, "uncaught client-side errors on the root route").toEqual([]);
  });

  test("root route loads all of its own assets successfully", async ({ page }) => {
    const badResponses: string[] = [];
    page.on("response", (response) => {
      if (response.status() >= 400) {
        badResponses.push(`${response.status()} ${response.url()}`);
      }
    });
    const failedRequests: string[] = [];
    page.on("requestfailed", (request) => {
      failedRequests.push(`${request.url()} (${request.failure()?.errorText ?? "unknown"})`);
    });

    await page.goto("/", { waitUntil: "load" });
    await page.waitForLoadState("networkidle");

    expect(badResponses, "subresources returned 4xx/5xx").toEqual([]);
    expect(failedRequests, "subresource requests failed").toEqual([]);
  });

  test("root route contacts no external origin (no Moira, no auth provider)", async ({
    page,
    baseURL,
  }) => {
    const origin = new URL(baseURL!).origin;
    const foreign = new Set<string>();

    page.on("request", (request) => {
      const url = request.url();
      if (url.startsWith("data:") || url.startsWith("blob:")) return;
      if (!url.startsWith(origin)) foreign.add(new URL(url).origin);
    });

    await page.goto("/", { waitUntil: "load" });
    await page.waitForLoadState("networkidle");

    expect(
      [...foreign],
      "the root route must render without calling Moira, an OAuth provider, or any CDN",
    ).toEqual([]);
  });

  /**
   * ==========================================================================
   * THE INVITATION PAGE TAKES A SECRET IN ITS URL — SO IT MUST TALK TO NOBODY
   * ==========================================================================
   *
   * `/invite/[token]` is unauthenticated and carries the invitation token in the
   * path. Moira's side is already bounded (prefix lookup before any Argon2 work,
   * so it is not a CPU-exhaustion oracle; an identical `invite_not_found` for a
   * wrong prefix and a wrong hash, so it is not a guessing oracle). The console's
   * side is this: the token is exchanged SERVER-SIDE on first load, and the page
   * must not hand the URL to anyone else.
   *
   * A single third-party request — an analytics beacon, a font, a CDN script —
   * sends the full URL in a `Referer` header by default, and the token with it.
   * There is no analytics in this console today, and this assertion is what keeps
   * "today" from quietly becoming "until someone adds a font".
   *
   * Deliberately the same assertion `/` already carries, extended rather than
   * copied: plan 09 §0.8.5 item 8 asks for exactly this.
   */
  test("the invitation page contacts no external origin (the token is in its URL)", async ({
    page,
    baseURL,
  }) => {
    const fixture = DYNAMIC_ROUTE_FIXTURES["/invite/[token]"];
    expect(
      fixture,
      "no fixture URL for /invite/[token] — this assertion would be testing nothing",
    ).toBeDefined();

    const origin = new URL(baseURL!).origin;
    const foreign = new Set<string>();

    page.on("request", (request) => {
      const url = request.url();
      if (url.startsWith("data:") || url.startsWith("blob:")) return;
      if (!url.startsWith(origin)) foreign.add(new URL(url).origin);
    });

    const response = await page.goto(fixture!, { waitUntil: "load" });
    // It must also RENDER rather than 404: an unusable invitation is a condition
    // the holder needs explained, and the a11y walker asserts `< 400` too.
    expect(response!.status()).toBeLessThan(400);
    await page.waitForLoadState("networkidle");

    expect(
      [...foreign],
      "the invitation token is in this page's URL, so any third-party request leaks it through " +
        "the Referer header",
    ).toEqual([]);
  });
});
