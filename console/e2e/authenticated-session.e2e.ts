import { expect, test } from "@playwright/test";

import { CONSOLE_MESSAGE_KEYS, t } from "../lib/i18n";
import {
  AUTH_OPERATOR,
  AUTH_PROVIDER_DISPLAY_NAME,
  AUTH_STORAGE_STATE,
  resetFixture,
} from "./support/authenticated-fixture";

/**
 * ============================================================================
 * A REAL SIGN-IN, IN A REAL BROWSER, AND THE STORAGE STATE IT PRODUCES
 * ============================================================================
 *
 * This is the `authenticated` project's dependency. It exists as its own project
 * so that a broken sign-in reports itself AS a broken sign-in, instead of as
 * several failing assertions about a screen the browser never reached.
 *
 * Nothing here is minted by the harness. The cookie the rest of the suite carries
 * comes from:
 *
 *   the console's own `POST /api/auth/sign-in/oauth2`  (the button posts it)
 *   -> a real `/authorize` on a real TLS socket        (`tests/support/mock-idp.ts`)
 *   -> a real authorization-code exchange with PKCE and a client secret
 *   -> the shipped `createConsoleAuth`, writing a session row to PostgreSQL.
 *
 * That distinction is the whole reason this project can assert things
 * `e2e/llm-settings.e2e.ts` cannot. In the scaffold environment every handler
 * answers 503 before the session check runs at all, so its refusal loop proves
 * "no 2xx and no Moira call reached" and — as its own header says — does NOT
 * prove that the SESSION is what separates a refusal from a success. Here it
 * does: the same request is made with and without this cookie.
 *
 * ============================================================================
 * WHAT THE CHEAPEST GREEN-LEAVING BREAKAGE WOULD BE
 * ============================================================================
 *
 * Landing on `/` proves a redirect happened; it does not prove a session exists,
 * because `/login` also 200s and a `callbackURL` that silently failed would leave
 * the browser on `/login`. So the assertion is on the CONSOLE HOME heading, which
 * only renders inside `(console)` — i.e. only past the layout's gate. A sign-in
 * that produced no usable cookie fails here rather than three specs later.
 */

test("an operator signs in through the browser and the session is stored", async ({ page }) => {
  // The fixture Moira starts from a freshly-migrated deployment. Done here, once,
  // rather than in the suite that follows, so the sign-in itself is never
  // competing with a reset.
  await resetFixture();

  await page.goto("/login", { waitUntil: "domcontentloaded" });
  expect(new URL(page.url()).pathname, "the sign-in page did not render").toBe("/login");

  // One provider is enabled in the fixture, and the anonymous sign-in-methods
  // projection supplies its display name — so `providerLabel` names the button
  // after it rather than falling back to the generic label. Asked for by that
  // name rather than as "the only button": a second provider appearing in the
  // fixture must fail here, not be silently clicked past.
  const button = page.getByRole("button", {
    name: t(CONSOLE_MESSAGE_KEYS.sign_in_button, { provider: AUTH_PROVIDER_DISPLAY_NAME }),
  });
  await expect(
    button,
    "no sign-in button rendered. `SignInPanel` renders buttons ONLY from resolved " +
      "configurations, so this means the fixture provider row did not resolve — check the " +
      "seeded client secret and the trusted-issuer string in e2e/support/authenticated-stack.ts.",
  ).toBeVisible();

  await button.click();

  // The whole redirect chain: console -> IdP `/authorize` -> console callback ->
  // `callbackURL`, which `SignInPanel` sets to `/`.
  await page.waitForURL((url) => url.pathname === "/", { timeout: 30_000 });

  // `/` is inside `(console)`, and its layout redirects an ungated visitor to
  // `/login`. Rendering its heading is therefore the session, not the redirect.
  await expect(
    page.getByRole("heading", { name: t(CONSOLE_MESSAGE_KEYS.page_home_title), level: 1 }),
    "landed on / without the console home heading — the sign-in produced no usable session",
  ).toBeVisible();

  // Recorded so the operator identity the rest of the suite acts as is a stated
  // fact rather than an assumption buried in the harness.
  expect(AUTH_OPERATOR.email.endsWith("@example.com")).toBe(true);

  await page.context().storageState({ path: AUTH_STORAGE_STATE });
});
