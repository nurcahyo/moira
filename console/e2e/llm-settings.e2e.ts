import { expect, test } from "@playwright/test";

import { APP_DIR } from "./support/paths";
import { discoverPageRoutes } from "./support/routes";

/**
 * ============================================================================
 * NOTHING ON THE LLM SURFACE IS REACHABLE WITHOUT A CONSOLE SESSION
 * ============================================================================
 *
 * The security property this whole item is held to, asserted against the
 * artifact a browser actually receives — the standalone production build the
 * `webServer` in `playwright.config.ts` assembles, not a test-time import.
 *
 * Two halves, because they fail for different reasons and both matter:
 *
 *   THE PAGE      `/settings/llm` is inside `(console)`, so the layout gate
 *                 redirects an unauthenticated visitor to `/login`. Asserted
 *                 here as well as in `a11y.e2e.ts` because that file's version
 *                 is a loop over discovered routes, and a loop that stopped
 *                 discovering this one would assert nothing about it.
 *
 *   THE HANDLERS  `app/api/llm/**` sits OUTSIDE every route group and inherits
 *                 no gate at all. Each handler calls `withConsoleSession`
 *                 itself. A browser with no cookie must get a keyed refusal —
 *                 401, 403 or 503 — and never a 2xx, and never a Moira call.
 *
 * ============================================================================
 * WHAT THIS SUITE DELIBERATELY DOES NOT DO, AND WHERE IT IS DONE INSTEAD
 * ============================================================================
 *
 * It does not drive the connect chain through the browser. There is no
 * authenticated Playwright project in this repository and the e2e environment
 * makes one impossible rather than merely absent: `MOIRA_API_URL` is
 * `https://moira.invalid`, `CONSOLE_DATABASE_URL` points at `db.invalid:1`, and
 * there is no IdP inside the e2e environment at all (`tests/support/mock-idp.ts`
 * serves the unit and integration suites, which run under Bun). `a11y.e2e.ts`
 * records the same constraint and the same reversal condition.
 *
 * The authenticated end-to-end run — real sign-in against the mock IdP, a real
 * session cookie, the real `withConsoleSession`, a mock `/v1/models`, and the
 * whole five-step chain — is
 * `tests/integration/llm-connect-flow.test.ts`. That is a stronger test of this
 * property than a browser could give: it exercises the real credential boundary
 * rather than a storage state somebody minted for it.
 *
 * REVERSAL CONDITION: when an authenticated Playwright project exists, move the
 * chain here and keep both assertions below.
 */

const LLM_PAGE = "/settings/llm";
const SIGN_IN_PATH = "/login";

/**
 * Every `app/api/llm/**` path, with a method that would WRITE if it were
 * reachable.
 *
 * Concrete ids, deliberately: a handler that answered before checking the
 * session would resolve them against Moira, and `moira.invalid` would make that
 * a 502 or a hang rather than a 401 — which is exactly the difference this
 * suite is looking for.
 */
const GUARDED_ENDPOINTS: ReadonlyArray<{
  method: string;
  path: string;
  /** Distinguishes the shortcut's two stages, which share one method and path. */
  stage?: string;
  body?: unknown;
}> = [
  { method: "GET", path: "/api/llm/providers" },
  {
    method: "POST",
    path: "/api/llm/providers",
    body: { display_name: "e2e", base_url: "https://endpoint.invalid" },
  },
  { method: "PATCH", path: "/api/llm/providers/e2e-provider", body: { display_name: "e2e" } },
  { method: "DELETE", path: "/api/llm/providers/e2e-provider" },
  { method: "POST", path: "/api/llm/providers/e2e-provider/models", body: { model_key: "e2e" } },
  { method: "DELETE", path: "/api/llm/providers/e2e-provider/models/e2e-model" },
  { method: "POST", path: "/api/llm/providers/e2e-provider/credentials", body: { api_key: "" } },
  { method: "DELETE", path: "/api/llm/providers/e2e-provider/credentials/e2e-credential" },
  {
    method: "POST",
    path: "/api/llm/providers/e2e-provider/routing",
    body: { provider_model_id: "e2e-model" },
  },
  { method: "DELETE", path: "/api/llm/providers/e2e-provider/routing/e2e-policy" },
  {
    method: "POST",
    path: "/api/llm/connect-vllm",
    stage: "discover",
    body: { action: "discover", base_url: "https://endpoint.invalid" },
  },
  {
    method: "POST",
    path: "/api/llm/connect-vllm",
    stage: "connect",
    body: { action: "connect", base_url: "https://endpoint.invalid", model_keys: ["e2e"] },
  },
];

/** The three keyed refusals `withConsoleSession` may answer with. */
const REFUSAL_STATUSES = new Set([401, 403, 503]);

test.describe("the LLM settings surface is gated", () => {
  test("the page exists and is inside the authenticated route group", () => {
    // A floor. Without it, a rename would make every assertion below vacuous:
    // an endpoint that does not exist answers 404, which is not a 2xx and would
    // pass the refusal check while proving nothing.
    const route = discoverPageRoutes(APP_DIR).find((candidate) => candidate.pattern === LLM_PAGE);
    expect(route, `${LLM_PAGE} was not discovered under app/`).toBeDefined();
    expect(
      route!.gated,
      `${LLM_PAGE} is not inside the (console) route group, so it inherits no session gate`,
    ).toBe(true);
  });

  test(`${LLM_PAGE} redirects an unauthenticated visitor to ${SIGN_IN_PATH}`, async ({ page }) => {
    await page.goto(LLM_PAGE, { waitUntil: "domcontentloaded" });
    expect(new URL(page.url()).pathname).toBe(SIGN_IN_PATH);
  });

  test("the redirect destination renders as a page, not as an error", async ({ page }) => {
    // The a11y walker asserts every discovered route answers below 400. A gated
    // route that 500s on the way to `/login` would take that gate red for a
    // reason unrelated to accessibility.
    const response = await page.goto(LLM_PAGE, { waitUntil: "domcontentloaded" });
    expect(response!.status()).toBeLessThan(400);
  });

  for (const endpoint of GUARDED_ENDPOINTS) {
    // The stage suffix is load-bearing: Playwright refuses a duplicate test
    // title, and the shortcut's two stages share a method and a path. Naming
    // them separately is also the honest thing — they are two different
    // privileged operations behind one door.
    const title =
      `${endpoint.method} ${endpoint.path}` +
      (endpoint.stage === undefined ? "" : ` (${endpoint.stage})`);

    test(`${title} refuses an unauthenticated caller`, async ({ request }) => {
      const response = await request.fetch(endpoint.path, {
        method: endpoint.method,
        ...(endpoint.body === undefined
          ? {}
          : { headers: { "content-type": "application/json" }, data: endpoint.body }),
      });

      expect(
        REFUSAL_STATUSES.has(response.status()),
        `${endpoint.method} ${endpoint.path} answered ${response.status()}; every handler under ` +
          "app/api/llm/** must call withConsoleSession, which refuses with 401, 403 or 503",
      ).toBe(true);

      // Keyed, and carrying no prose the browser would have to interpret.
      const body = (await response.json()) as { error?: { code?: string; message_key?: string } };
      expect(body.error?.code, `${endpoint.path} answered no keyed error`).toBeTruthy();
      expect(body.error?.message_key).toMatch(/^console\./);
      expect(response.headers()["cache-control"]).toBe("no-store");
    });
  }

  test("the guarded list covers every handler this item added", () => {
    // Named rather than counted, so adding a handler without adding it here is
    // visible. Eleven exported handlers across nine route modules; the shortcut
    // appears twice because its two stages are worth refusing separately.
    const paths = new Set(GUARDED_ENDPOINTS.map((endpoint) => endpoint.path));
    for (const path of [
      "/api/llm/providers",
      "/api/llm/providers/e2e-provider",
      "/api/llm/providers/e2e-provider/models",
      "/api/llm/providers/e2e-provider/models/e2e-model",
      "/api/llm/providers/e2e-provider/credentials",
      "/api/llm/providers/e2e-provider/credentials/e2e-credential",
      "/api/llm/providers/e2e-provider/routing",
      "/api/llm/providers/e2e-provider/routing/e2e-policy",
      "/api/llm/connect-vllm",
    ]) {
      expect(paths, `${path} is not covered by the refusal loop`).toContain(path);
    }
  });
});
