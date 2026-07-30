import { expect, test } from "@playwright/test";

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
});
