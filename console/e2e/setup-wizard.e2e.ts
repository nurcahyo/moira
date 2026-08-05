// `/setup`, with the setup window ACTUALLY OPEN — every wizard step rendered by
// a browser, audited by axe, and scanned for the credentials it handles.
//
// ============================================================================
// THE BLOCKER THIS FILE CLOSES (issue #71, AC4)
// ============================================================================
//
// `a11y.e2e.ts` has always had a green line reading "no critical or serious axe
// violations on /setup", and `secret-leak.e2e.ts` has always had "no secret
// reaches the browser on /setup". Both were true and both were nearly empty:
// the main e2e console runs with NO `MOIRA_SYSTEM_KEY`, so `withSetupWindow`
// answered `404 setup_unavailable`, `SetupWizard` early-returned a two-element
// refusal panel, and that panel is what both lines audited. The welcome step,
// the auth-settings FORM (the largest interactive surface in the console), the
// sign-in surface and the done step had never been rendered by the e2e suite at
// all.
//
// This file runs against the setup fixture console instead — the same
// standalone production artifact, given a bootstrap system key and a stub Moira
// on loopback TLS. See `e2e/support/setup-fixture.ts` for every design decision
// behind that stack. The main console is untouched, so every existing
// assertion still describes the server it was written against.
//
// ============================================================================
// WHY THE TESTS ARE SERIAL, AND WHY THAT IS NOT A SMELL
// ============================================================================
//
// A first-run wizard is a state machine over ONE deployment: fresh, then
// provisioned, then claimed. There is no way to observe the sign-in step
// without having provisioned, and no way to provision twice from scratch
// without resetting the deployment. So the file walks one deployment forward,
// in order, and says so — rather than four independent tests each secretly
// depending on whichever ran first.
//
// The provisioning in the middle is REAL: the browser fills the shipped form,
// the shipped BFF route runs, `runSetupProvisioning` performs its four ordered
// operations against the stub over TLS with the bootstrap key attached, and the
// OAuth client secret is sealed into the fixture console's own PostgreSQL store
// by `PostgresConsoleSecretStore`. The wizard then advances because
// `isProvisioningComplete` holds — not because a test told it to.
//
// ============================================================================
// WHAT IS STILL NOT AUDITED HERE, DECLARED
// ============================================================================
//
// `SignInClaimStep`'s `stage === "claim"` sub-surface. Reaching it needs a
// Better Auth session, which needs a completed OAuth round trip against a mock
// IdP inside the e2e environment (issue #72). The last test below asserts
// POSITIVELY that the fixture stops at `sign_in`, so this gap is pinned: when
// the harness lands and the wizard starts reaching `claim`, that assertion
// fails and whoever lands it has to audit the new surface rather than discover
// months later that nobody did.

import { expect, request, test, type Page } from "@playwright/test";

import { CONSOLE_CATALOG } from "../lib/i18n/catalog.en";
import { CONSOLE_MESSAGE_KEYS as K } from "../lib/i18n/keys";
import { auditPage } from "./support/axe";
import { attachLeakTap, type CapturedBlob } from "./support/leak-tap";
import { describeLeaks, forbiddenValues, scanForLeaks, type Leak } from "./support/secrets";
import {
  SETUP_FIXTURE_CLIENT_SECRET,
  SETUP_FIXTURE_CONTROL_PATH,
  SETUP_FIXTURE_MOIRA_URL,
  SETUP_FIXTURE_PROVIDER,
} from "./support/setup-fixture";

/** English, because the fixture console serves the English catalog. */
function copy(key: string): string {
  return CONSOLE_CATALOG[key as keyof typeof CONSOLE_CATALOG].message;
}

const needles = forbiddenValues();

/**
 * Put the stub Moira's deployment into a known state.
 *
 * Goes over the network from the RUNNER, not from the browser and not through
 * the console: the control surface is the spec's, and the console has neither a
 * route that reaches it nor any knowledge that it exists.
 *
 * `rejectUnauthorized: false` is unavailable to `fetch`, so the certificate is
 * trusted the only way a Playwright runner can — by asking Playwright's own
 * request context to ignore it. That relaxation applies to THIS control call
 * only; the console's requests to the same stub go through a real chain
 * validation against `NODE_EXTRA_CA_CERTS`.
 */
async function resetDeployment(claimed: boolean): Promise<void> {
  const context = await request.newContext({ ignoreHTTPSErrors: true });
  try {
    const response = await context.post(`${SETUP_FIXTURE_MOIRA_URL}${SETUP_FIXTURE_CONTROL_PATH}`, {
      data: { claimed },
    });
    expect(response.status(), "the stub Moira did not accept the fixture reset").toBe(200);
  } finally {
    await context.dispose();
  }
}

/**
 * Every scan this file performs, in one place so no step gets a weaker one.
 *
 * Two surfaces, and the omission of a third is deliberate:
 *
 *   1. EVERYTHING THE BROWSER RECEIVED — HTML, the RSC flight payload, JS
 *      chunks, JSON, plus console output and uncaught errors. This is the
 *      channel a leak actually travels down, and it is the same tap
 *      `secret-leak.e2e.ts` uses.
 *   2. VISIBLE COPY (`body.innerText`). A screenshot, a support paste or a
 *      copied error report carries what a human can see.
 *
 * NOT the serialised DOM. `secret-leak.e2e.ts` scans it because none of its
 * routes has a field an operator types a credential into; this file's
 * auth-settings step does, and React reflects a controlled input's value onto
 * the `value` attribute. Asserting the secret is absent from `page.content()`
 * here would be asserting that the client-secret field cannot be typed into —
 * the operator's own keystrokes in the operator's own browser are not the leak
 * the gate is for. What must never happen is the console SENDING it back, and
 * (1) is what proves it does not.
 */
async function assertNoSecretReachedTheBrowser(
  page: Page,
  tap: { drain: () => Promise<CapturedBlob[]> },
  label: string,
): Promise<void> {
  const leaks: Leak[] = [];
  for (const blob of await tap.drain()) {
    leaks.push(...scanForLeaks(blob.where, blob.content, needles));
  }
  leaks.push(
    ...scanForLeaks(`visible copy of ${label}`, await page.locator("body").innerText(), needles),
  );
  expect(
    leaks,
    `secret material observable from the browser on ${label}:\n${describeLeaks(leaks)}`,
  ).toEqual([]);
}

/** Fill the auth-settings form with the fixture provider and save. */
async function provisionThroughTheForm(page: Page): Promise<void> {
  await page.fill('input[name="display_name"]', SETUP_FIXTURE_PROVIDER.displayName);
  await page.fill('input[name="client_id"]', SETUP_FIXTURE_PROVIDER.clientId);
  await page.fill('input[name="client_secret"]', SETUP_FIXTURE_CLIENT_SECRET);
  await page.fill('input[name="discovery_url"]', SETUP_FIXTURE_PROVIDER.discoveryUrl);
  await page.fill('input[name="allowed_email_domains"]', SETUP_FIXTURE_PROVIDER.allowedDomain);
  await page.getByRole("button", { name: copy(K.setup_auth_submit) }).click();
}

test.describe.configure({ mode: "serial" });

test.describe("the setup wizard, on a deployment whose setup window is open", () => {
  test("the fixture opens the window at all — otherwise every audit below is vacuous", async ({
    page,
  }) => {
    // The floor. Without it, a fixture that silently regressed to
    // `setup_unavailable` would leave every axe assertion in this file passing
    // against the same refusal panel `a11y.e2e.ts` already audits — which is
    // exactly the failure this file was written to end.
    await resetDeployment(false);
    const response = await page.goto("/setup", { waitUntil: "domcontentloaded" });
    expect(response!.status()).toBeLessThan(400);

    await expect(
      page.getByRole("heading", { name: copy(K.setup_unavailable_heading) }),
      "the fixture console answered `setup_unavailable`: it is missing MOIRA_SYSTEM_KEY, or its " +
        "stub Moira is unreachable. Every wizard-step audit in this file would then be auditing " +
        "the refusal panel.",
    ).toHaveCount(0);
    await expect(page.getByRole("heading", { name: copy(K.setup_welcome_heading) })).toBeVisible();
  });

  test("welcome step: no critical or serious axe violations, and no secret", async ({
    page,
  }, testInfo) => {
    const tap = attachLeakTap(page);
    await page.goto("/setup", { waitUntil: "load" });
    await expect(page.getByRole("heading", { name: copy(K.setup_welcome_heading) })).toBeVisible();

    const audit = await auditPage(page, testInfo, "setup-welcome");
    expect(
      audit.ids,
      `critical/serious accessibility violations on the wizard's welcome step:\n${audit.report}`,
    ).toEqual([]);

    await assertNoSecretReachedTheBrowser(page, tap, "the wizard's welcome step");
  });

  test("auth-settings step: no critical or serious axe violations, and no secret", async ({
    page,
  }, testInfo) => {
    const tap = attachLeakTap(page);
    await page.goto("/setup", { waitUntil: "load" });
    await page.getByRole("button", { name: copy(K.setup_welcome_continue) }).click();
    await expect(page.getByRole("heading", { name: copy(K.setup_auth_heading) })).toBeVisible();

    // The form is filled before the audit, deliberately: axe on an empty form
    // does not see the error/description wiring that only exists once a field
    // carries a value, and this is the console's largest interactive surface.
    await page.fill('input[name="display_name"]', SETUP_FIXTURE_PROVIDER.displayName);
    await page.fill('input[name="client_id"]', SETUP_FIXTURE_PROVIDER.clientId);
    await page.fill('input[name="client_secret"]', SETUP_FIXTURE_CLIENT_SECRET);

    const audit = await auditPage(page, testInfo, "setup-auth-settings");
    expect(
      audit.ids,
      `critical/serious accessibility violations on the wizard's auth-settings step:\n${audit.report}`,
    ).toEqual([]);

    // The typed client secret is a `SENTINEL_ENV` value, so it is already one of
    // `forbiddenValues()`'s needles: this asserts the form does not echo it into
    // the document, an RSC payload, or the console.
    await assertNoSecretReachedTheBrowser(page, tap, "the wizard's auth-settings step");
  });

  test("the auth-settings form's field errors are announced, and audited", async ({
    page,
  }, testInfo) => {
    // The refusal shape is a11y-relevant in its own right (`role="alert"`,
    // per-field error text, the `aria-describedby` note on the allow-list) and
    // it is not reachable from the happy path.
    await page.goto("/setup", { waitUntil: "load" });
    await page.getByRole("button", { name: copy(K.setup_welcome_continue) }).click();
    await page.getByRole("button", { name: copy(K.setup_auth_submit) }).click();

    await expect(page.getByRole("alert").first()).toBeVisible();

    const audit = await auditPage(page, testInfo, "setup-auth-settings-invalid");
    expect(
      audit.ids,
      `critical/serious accessibility violations on the refused auth-settings form:\n${audit.report}`,
    ).toEqual([]);
  });

  test("a real provision advances the wizard to the sign-in step, which is audited", async ({
    page,
  }, testInfo) => {
    const tap = attachLeakTap(page);
    await page.goto("/setup", { waitUntil: "load" });
    await page.getByRole("button", { name: copy(K.setup_welcome_continue) }).click();
    await expect(page.getByRole("heading", { name: copy(K.setup_auth_heading) })).toBeVisible();

    await provisionThroughTheForm(page);

    // Nothing here nudges the cursor: `AuthSettingsStep` only reports the state
    // Moira confirmed, and `SetupWizard` moves to `sign_in` because
    // `isProvisioningComplete` holds — which includes `consoleSecretStored`,
    // i.e. the secret really was sealed in the fixture console's database.
    await expect(
      page.getByRole("heading", { name: copy(K.setup_sign_in_heading) }),
      "provisioning did not complete against the stub Moira. Check the fixture console's log: " +
        "the four ordered operations (trusted issuer, provider, console secret, enable) each " +
        "report their own failure.",
    ).toBeVisible();

    const audit = await auditPage(page, testInfo, "setup-sign-in");
    expect(
      audit.ids,
      `critical/serious accessibility violations on the wizard's sign-in step:\n${audit.report}`,
    ).toEqual([]);

    await assertNoSecretReachedTheBrowser(page, tap, "the wizard's sign-in step");
  });

  test("a revisit rehydrates the provisioned state, and the way back is audited", async ({
    page,
  }, testInfo) => {
    // The OAuth round trip is a full navigation away from `/setup` and back, so
    // the server-derived rehydration is what carries the wizard across it. This
    // is that reload, and it lands on `sign_in` without anything client-side
    // remembering.
    await page.goto("/setup", { waitUntil: "load" });
    await expect(page.getByRole("heading", { name: copy(K.setup_sign_in_heading) })).toBeVisible();

    // ...and the way back, which is the only place a mistyped discovery URL,
    // client id or client secret can still be corrected.
    await page.getByRole("button", { name: copy(K.setup_sign_in_edit_settings) }).click();
    await expect(page.getByRole("heading", { name: copy(K.setup_auth_heading) })).toBeVisible();

    // On a revisit the auth-settings step also renders the EXISTING provider
    // row with its masked "configured" copy (AC3), which nothing audited before.
    await expect(
      page.getByRole("region", { name: copy(K.setup_auth_existing_heading) }),
    ).toBeVisible();
    // `exact` because the section's own heading ("Already configured in Moira")
    // is a substring match for the masked row copy ("Configured").
    await expect(
      page.getByText(copy(K.setup_auth_existing_configured), { exact: true }),
    ).toBeVisible();

    const audit = await auditPage(page, testInfo, "setup-auth-settings-revisit");
    expect(
      audit.ids,
      `critical/serious accessibility violations on the revisited auth-settings step:\n${audit.report}`,
    ).toEqual([]);
  });

  test("the claim sub-surface is NOT reached here, and that gap stays declared", async ({
    page,
  }) => {
    // See the header note. `reachableSetupStep` returns `claim` only with a
    // signed-in identity, which needs an OAuth round trip this environment
    // cannot perform. Asserted rather than left silent: when issue #72's mock
    // IdP lands, this fails and the claim surface has to be audited here.
    await page.goto("/setup", { waitUntil: "load" });
    await expect(page.getByRole("heading", { name: copy(K.setup_sign_in_heading) })).toBeVisible();
    await expect(
      page.getByRole("button", { name: copy(K.setup_claim_button) }),
      "the wizard reached the claim sub-surface. Audit it here and delete the declared gap in " +
        "a11y.e2e.ts's header.",
    ).toHaveCount(0);
  });

  test("done step: a deployment Moira already reports claimed", async ({ page }, testInfo) => {
    // The terminal step, from the shape that does NOT need a session: Moira
    // answers `claimed: true`, the BFF answers `409 setup_already_claimed`, and
    // the page renders `kind: "claimed"` — which seeds the cursor at `done`.
    await resetDeployment(true);
    const tap = attachLeakTap(page);
    const response = await page.goto("/setup", { waitUntil: "load" });
    expect(response!.status()).toBeLessThan(400);

    await expect(page.getByRole("heading", { name: copy(K.setup_done_heading) })).toBeVisible();
    await expect(page.getByRole("link", { name: copy(K.setup_done_open_console) })).toBeVisible();

    const audit = await auditPage(page, testInfo, "setup-done");
    expect(
      audit.ids,
      `critical/serious accessibility violations on the wizard's done step:\n${audit.report}`,
    ).toEqual([]);

    await assertNoSecretReachedTheBrowser(page, tap, "the wizard's done step");

    // Leave the fixture deployment fresh for a re-run against a reused server.
    await resetDeployment(false);
  });
});
