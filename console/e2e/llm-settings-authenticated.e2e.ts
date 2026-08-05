import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";

import { CONSOLE_MESSAGE_KEYS, t } from "../lib/i18n";
import {
  DISCOVERABLE_MODEL_KEYS,
  MOIRA_IDS,
  UNREACHABLE_ENDPOINT_URL,
  readFixture,
  resetFixture,
  resetRecording,
  type RecordedMoiraRequest,
} from "./support/authenticated-fixture";

/**
 * ============================================================================
 * `/settings/llm`, DRIVEN BY A BROWSER THAT IS ACTUALLY SIGNED IN — ISSUE #75
 * ============================================================================
 *
 * The suite `a11y.e2e.ts` and `e2e/llm-settings.e2e.ts` both named as their
 * REVERSAL CONDITION: "when an authenticated Playwright project exists". It does
 * now (`e2e/support/authenticated-stack.ts`), and this is the file it was for.
 *
 * What is REAL: the standalone artifact the Dockerfile ships; a session minted by
 * the shipped `createConsoleAuth` from a real authorization-code exchange against
 * a real TLS-terminated OIDC provider; the console's own PostgreSQL; a real
 * OpenAI-compatible endpoint on a real socket. What is MOCKED: Moira's HTTP
 * surface, and the identity provider's identity.
 *
 * ============================================================================
 * WHAT IS NOT DUPLICATED FROM `tests/integration/llm-connect-flow.test.ts`
 * ============================================================================
 *
 * That file drives `POST /api/llm/connect-vllm` directly and owns the negative
 * space around it — an admin-grant refusal, a second run creating nothing, the
 * projection that keeps a fingerprint off the screen. It cannot answer whether a
 * HUMAN can reach any of it: it never renders a page, never clicks anything, and
 * cannot see that the panel offers what discovery found or that the page it
 * refreshes afterwards agrees with the trace it printed.
 *
 * This file is the browser half, and it asserts the ORDER of the writes at the
 * wire rather than their existence — see the note above that assertion.
 */

const LLM_PAGE = "/settings/llm";
const SIGN_IN_PATH = "/login";

const AXE_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];
const BLOCKING_IMPACTS = new Set(["critical", "serious"]);

/** The first model the endpoint advertises. `ConnectVllmPanel` pre-selects it. */
const FIRST_MODEL = DISCOVERABLE_MODEL_KEYS[0];

/**
 * The whole chain, in the order Moira must see it.
 *
 * ============================================================================
 * WHY THIS IS AN ORDERED EQUALITY AND NOT A SET
 * ============================================================================
 *
 * Each row is a precondition for the next: a `provider_model` needs a provider
 * id, a `provider_credential` names the provider it belongs to, and a
 * `routing_policy` names BOTH the provider and the model it routes to. A chain
 * that created them in a different order does not produce the same deployment in
 * a different sequence — it produces a 4xx from Moira partway through, and a
 * half-built provider the operator then has to unpick.
 *
 * So a test that only checked "four rows exist at the end" would pass on an
 * implementation Moira rejects, and would keep passing for as long as the
 * fixture answered 201 to anything. This asserts the recorded request sequence,
 * reads included: the reuse-first lookup before each create is what makes a
 * second click continue rather than duplicate, and an implementation that
 * dropped the lookups would still end up with four rows.
 */
const CONNECT_CHAIN: readonly string[] = [
  "GET /api/v1/admin/providers",
  "POST /api/v1/admin/providers",
  `GET /api/v1/admin/providers/${MOIRA_IDS.provider}/models`,
  `POST /api/v1/admin/providers/${MOIRA_IDS.provider}/models`,
  "GET /api/v1/admin/provider-credentials",
  "POST /api/v1/admin/provider-credentials",
  "GET /api/v1/admin/routes",
  "GET /api/v1/admin/routing-policies",
  "POST /api/v1/admin/routing-policies",
];

/** The four writes, in the order the chain must perform them. */
const CHAIN_WRITES: readonly string[] = CONNECT_CHAIN.filter((route) => route.startsWith("POST "));

function routesOf(requests: readonly RecordedMoiraRequest[]): string[] {
  return requests.map((request) => request.route);
}

/**
 * Every recorded call that could have CHANGED something.
 *
 * Asserted on instead of the whole recording wherever a test's claim is "nothing
 * was created": the App Router prefetches `/settings/llm` on hydration and that
 * prefetch is a full server render, so the READS in a window are not this
 * suite's to predict. The writes are.
 */
function writesIn(requests: readonly RecordedMoiraRequest[]): string[] {
  return requests.filter((request) => request.method !== "GET").map((request) => request.route);
}

/**
 * Type the fixture endpoint's ORIGIN, so the `/v1` canonicalisation is visible.
 *
 * ============================================================================
 * IT REFUSES A NON-LOOPBACK VALUE, AND IT CHECKS THE VALUE SURVIVED
 * ============================================================================
 *
 * The field arrives PRE-FILLED with `LOCAL_VLLM_BASE_URL`, which names a REAL
 * deployment. Every test here types over it — and the first version of this
 * helper did the typing and nothing else, which is how the suite came to probe
 * that deployment from a test run.
 *
 * REPRODUCED. `page.goto(..., { waitUntil: "domcontentloaded" })` returns before
 * React hydrates. `ConnectVllmPanel` renders a CONTROLLED input
 * (`value={baseUrl}`, `useState(defaultBaseUrl)`), so a `fill()` that lands
 * before hydration writes the DOM node and nothing else: hydration then restores
 * `defaultBaseUrl` and the typed address is gone with no error anywhere.
 * Pressing Discover next sent `https://local-llm.motrait.com` to the BFF, which
 * probed it — and because that deployment really does serve `Qwen3-4B`, the
 * `toBeChecked()` assertion on the first advertised model PASSED. The run went
 * green while contacting production. It surfaced only when the recorded
 * `POST /api/v1/admin/providers` body was compared against the fixture URL.
 *
 * So the helper now does three things instead of one:
 *
 *   1. refuses a non-loopback value outright — the caller's mistake, caught
 *      before anything is typed;
 *   2. waits for `networkidle` FIRST, so React owns the input by the time it is
 *      filled and `fill()` runs the `onChange` that updates the state;
 *   3. asserts the field still holds the value afterwards, so a fill that is
 *      discarded again — by hydration or by anything else — fails HERE, loudly,
 *      instead of redirecting the probe to a real host.
 *
 * Step 2 is the fix and step 3 is the alarm, in that order and not the other way
 * round. Removing step 2 and keeping step 3 was run as a negative control: three
 * repeats of the chain test, three failures, and in all three the `toHaveValue`
 * in step 3 PASSED — hydration reverted the field after the assertion had
 * already observed it. The alarm alone catches nothing.
 *
 * `authenticated-stack.ts` makes the matching assertion from the other side,
 * about the socket it bound.
 */
async function fillEndpoint(page: import("@playwright/test").Page, value: string): Promise<void> {
  const host = new URL(value).hostname;
  if (host !== "127.0.0.1" && host !== "localhost") {
    throw new Error(
      `refusing to type ${value} into the endpoint field: only a loopback address the harness ` +
        "itself bound may be probed. The field's pre-filled default names a real deployment, and " +
        "a suite that contacts it is testing someone else's uptime.",
    );
  }
  await page.waitForLoadState("networkidle");
  const field = page.getByLabel(t(CONSOLE_MESSAGE_KEYS.llm_connect_endpoint_label));
  await field.fill(value);
  await expect(
    field,
    "the endpoint field did not keep the address that was typed into it. The panel's input is " +
      "controlled by React state, so a fill that lands before hydration is silently reverted to " +
      "the PRE-FILLED DEFAULT — which names a real deployment. Pressing Discover after that " +
      "probes production and can still go green.",
  ).toHaveValue(value);
}

async function runAxe(
  page: import("@playwright/test").Page,
  label: string,
  testInfo: import("@playwright/test").TestInfo,
) {
  const results = await new AxeBuilder({ page }).withTags(AXE_TAGS).analyze();
  await testInfo.attach(`axe-${label}.json`, {
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
    `critical/serious accessibility violations on ${LLM_PAGE} (${label}):\n${report}`,
  ).toEqual([]);
}

/**
 * ============================================================================
 * SERIAL, BECAUSE THERE IS ONE FIXTURE AND IT IS MUTABLE
 * ============================================================================
 *
 * The stack runs ONE fixture Moira and ONE recorder. `fullyParallel: true` in
 * the config would let two of these tests reset each other's recording midway
 * and produce a failure that reproduces once in ten runs. Serial is not a
 * concession here — a suite whose subject is the ORDER of a sequence of writes
 * cannot share a recorder with anything.
 *
 * AND `--repeat-each` DOES NOT WORK ON THIS FILE, which is worth writing down
 * because the failure it produces looks like a real ordering bug. Observed while
 * checking the fix in `fillEndpoint`: `--repeat-each=3` fails the chain test with
 * a window containing two page renders' worth of reads and none of the four
 * writes — the reuse-first lookups found rows a previous repeat had left, so the
 * chain correctly created nothing. `beforeEach`'s `resetFixture()` cannot close
 * that: a repeat begins while the previous one's `router.refresh()` is still in
 * flight against the same fixture. Repeat the whole `playwright test`
 * invocation instead; three consecutive runs were used to confirm the fix.
 */
test.describe.configure({ mode: "serial" });

/**
 * A freshly-migrated deployment before every test, in BOTH blocks: the
 * unauthenticated block asserts that nothing was created, and that claim is only
 * worth making against a fixture that started empty.
 */
test.beforeEach(async () => {
  await resetFixture();
});

/* ========================================================================== */
/* 1. Unreachable without a session                                           */
/* ========================================================================== */

test.describe("without a console session", () => {
  // The project's storage state, dropped. Everything in this block is a browser
  // that has never signed in — against a console that COULD have admitted it,
  // which is the difference from `e2e/llm-settings.e2e.ts`.
  test.use({ storageState: { cookies: [], origins: [] } });

  test(`${LLM_PAGE} redirects to ${SIGN_IN_PATH}`, async ({ page }) => {
    const response = await page.goto(LLM_PAGE, { waitUntil: "domcontentloaded" });

    // Asserted as a redirect, not as "the heading is missing". A page that
    // rendered the shell and hid its contents would pass the second and fail
    // this one.
    expect(new URL(page.url()).pathname).toBe(SIGN_IN_PATH);
    // And it arrives as a page, because the a11y walker holds every route to
    // answering below 400 on the way.
    expect(response!.status()).toBeLessThan(400);
    await expect(
      page.getByRole("heading", { name: t(CONSOLE_MESSAGE_KEYS.llm_page_title) }),
    ).toHaveCount(0);
  });

  test("the shortcut endpoint refuses with 401 no_session — not a 503", async ({ request }) => {
    // ======================================================================
    // THE ASSERTION THE SCAFFOLD SUITE CANNOT MAKE
    // ======================================================================
    //
    // `e2e/llm-settings.e2e.ts` accepts 401, 403 OR 503, and its own header
    // states why that makes it weaker than it reads: in that environment
    // `MOIRA_API_URL` is unresolvable, so `withConsoleSession` returns 503
    // BEFORE the session check runs at all, and the loop proves only that no
    // 2xx and no Moira call escaped.
    //
    // Here Moira resolves and the console is fully configured, so 503 is not
    // available as an excuse. `no_session` is the check itself, named.
    //
    // ======================================================================
    // BOTH STAGES ARE PROBED, AND WITH ADDRESSES THAT WOULD WORK
    // ======================================================================
    //
    // The first version sent `base_url: UNREACHABLE_ENDPOINT_URL` and then
    // asserted that the fixture had recorded no probe and had created no rows.
    // Neither assertion could fail. `127.0.0.1:1` is refused by the kernel, so
    // even with the session gate DELETED the mock endpoint would have recorded
    // nothing; and `discover` writes nothing to Moira on any path, so the row
    // counts stay at zero whatever happens. Two green lines that described the
    // address in the request body rather than the gate under test.
    //
    // So `discover` is sent the address of the mock this harness STARTED — a
    // probe that reaches it is recorded, which is what makes "no probe left the
    // deployment" a claim about the gate — and `connect` is sent a body that
    // would build the whole chain, which is what makes the row counts one.
    const fixture = await readFixture();

    const discover = await request.fetch("/api/llm/connect-vllm", {
      method: "POST",
      headers: { "content-type": "application/json" },
      data: { action: "discover", base_url: fixture.vllmOrigin },
    });

    expect(discover.status()).toBe(401);
    const body = (await discover.json()) as { error?: { code?: string; message_key?: string } };
    expect(body.error?.code).toBe("no_session");
    expect(body.error?.message_key).toMatch(/^console\./);
    expect(discover.headers()["cache-control"]).toBe("no-store");

    const connect = await request.fetch("/api/llm/connect-vllm", {
      method: "POST",
      headers: { "content-type": "application/json" },
      data: {
        action: "connect",
        base_url: fixture.vllmOrigin,
        display_name: "unauthenticated-caller",
        model_keys: [DISCOVERABLE_MODEL_KEYS[0]],
      },
    });

    expect(connect.status()).toBe(401);
    expect(
      ((await connect.json()) as { error?: { code?: string } }).error?.code,
      "the connect stage answered something other than the session refusal",
    ).toBe("no_session");

    // Refused before anything was contacted: no probe left the deployment, and
    // no admin call was made to be refused.
    const after = await readFixture();
    expect(after.vllmRequests, "an unauthenticated caller made the BFF probe a host").toEqual([]);
    expect(
      writesIn(after.moiraRequests),
      "an unauthenticated caller reached Moira's admin plane",
    ).toEqual([]);
    expect(after.rows).toEqual({
      providers: 0,
      providerModels: 0,
      providerCredentials: 0,
      routingPolicies: 0,
    });
  });
});

/* ========================================================================== */
/* 2. With a session                                                          */
/* ========================================================================== */

test.describe("with a console session", () => {
  // The storage state is deliberately NOT reset between tests: re-signing-in per
  // test would make every one of them a re-test of sign-in, which is
  // `authenticated-session.e2e.ts`'s job.

  test("the page renders for a signed-in operator", async ({ page }) => {
    await page.goto(LLM_PAGE, { waitUntil: "domcontentloaded" });

    // The URL, asserted. `page.goto` follows redirects, so a gated page that
    // sent the browser to `/login` would otherwise be audited and asserted
    // under this page's name — the W5-B6 defect `a11y.e2e.ts` records.
    expect(new URL(page.url()).pathname).toBe(LLM_PAGE);
    await expect(
      page.getByRole("heading", { name: t(CONSOLE_MESSAGE_KEYS.llm_page_title), level: 1 }),
    ).toBeVisible();

    // The two failure banners the page renders instead of the panels. Both are
    // `role="alert"`, and either one means the fixture is wrong rather than the
    // console: `llm_load_failed` is an unreachable Moira, and
    // `llm_general_route_missing` is a deployment with no seeded route.
    await expect(page.getByText(t(CONSOLE_MESSAGE_KEYS.llm_load_failed))).toHaveCount(0);
    await expect(page.getByText(t(CONSOLE_MESSAGE_KEYS.llm_general_route_missing))).toHaveCount(0);
  });

  test("the connect panel discovers models from the mock endpoint", async ({ page }) => {
    const fixture = await readFixture();
    await page.goto(LLM_PAGE, { waitUntil: "domcontentloaded" });

    // The ORIGIN, deliberately: `canonicalOpenAiBaseUrl` appends `/v1`, and
    // "discovery worked" and "the provider works" must not be two different
    // addresses. Typing the canonical form would hide a regression that dropped
    // the suffix.
    await fillEndpoint(page, fixture.vllmOrigin);
    await resetRecording(page);
    await page
      .getByRole("button", { name: t(CONSOLE_MESSAGE_KEYS.llm_connect_discover_submit) })
      .click();

    // What it FOUND is what it OFFERS. A checkbox per advertised model id, and
    // no others: an implementation that offered a hard-coded list, or that
    // silently kept the previous answer, fails here.
    await expect(page.getByRole("checkbox")).toHaveCount(DISCOVERABLE_MODEL_KEYS.length);
    for (const modelKey of DISCOVERABLE_MODEL_KEYS) {
      await expect(page.getByRole("checkbox", { name: modelKey })).toBeVisible();
    }
    await expect(page.getByRole("checkbox", { name: FIRST_MODEL })).toBeChecked();

    // The canonical base came BACK from the server and replaced what was typed.
    await expect(page.getByLabel(t(CONSOLE_MESSAGE_KEYS.llm_connect_endpoint_label))).toHaveValue(
      fixture.vllmBaseUrl,
    );

    const after = await readFixture();
    // The probe went out from the BFF, once, to the mock this harness started —
    // never to `https://local-llm.motrait.com`, and never from the browser.
    expect(after.vllmRequests).toEqual(["GET /v1/models"]);
    // Discovery is authorised by an admin-plane READ — `requireAdminGrant`, made
    // BEFORE the address is even parsed — and it writes nothing.
    expect(
      routesOf(after.moiraRequests),
      "discovery did not read the admin grant that authorises it",
    ).toContain("GET /api/v1/admin/providers");
    expect(writesIn(after.moiraRequests), "a discovery probe wrote to Moira").toEqual([]);
    expect(after.rows).toEqual({
      providers: 0,
      providerModels: 0,
      providerCredentials: 0,
      routingPolicies: 0,
    });
  });

  test("completing the chain creates provider, model, credential, policy IN THAT ORDER", async ({
    page,
  }) => {
    const fixture = await readFixture();
    await page.goto(LLM_PAGE, { waitUntil: "domcontentloaded" });
    await fillEndpoint(page, fixture.vllmOrigin);
    await page
      .getByRole("button", { name: t(CONSOLE_MESSAGE_KEYS.llm_connect_discover_submit) })
      .click();
    await expect(page.getByRole("checkbox", { name: FIRST_MODEL })).toBeChecked();

    // WHICH HOST ANSWERED, before the recording that would forget it is cleared.
    //
    // `toBeChecked()` above cannot tell the fixture endpoint from the shipped
    // default: `https://local-llm.motrait.com` also serves `Qwen3-4B`, so a
    // probe that went there advertises the same first model and satisfies the
    // same assertion. The mock's own request log is the only thing that names
    // the host, and after `resetRecording` it is gone.
    const probed = await readFixture();
    expect(
      probed.vllmRequests,
      "discovery did not reach the mock endpoint this harness started, so it reached something " +
        "else — see fillEndpoint for how that happens without any assertion noticing.",
    ).toEqual(["GET /v1/models"]);

    // The recording starts at the click this test is about. Rows are NOT reset:
    // an ordering assertion must not be bought by wiping the state the click is
    // supposed to find.
    await resetRecording(page);
    await page.getByRole("button", { name: t(CONSOLE_MESSAGE_KEYS.llm_connect_submit) }).click();

    await expect(page.getByText(t(CONSOLE_MESSAGE_KEYS.llm_connect_done))).toBeVisible();

    // ---- the order, at the wire ------------------------------------------
    const after = await readFixture();
    const recorded = routesOf(after.moiraRequests);
    expect(
      recorded.slice(0, CONNECT_CHAIN.length),
      "the connect chain did not reach Moira in the order the seed script documents. Each row " +
        "is a precondition for the next, so a different order is a 4xx partway through and a " +
        "half-built provider — not the same deployment assembled differently.",
    ).toEqual(CONNECT_CHAIN);

    // `slice` above pins the sequence; this pins that nothing ran twice in the
    // refresh that follows. Together they are an exact statement, without
    // making the assertion hostage to how many reads a `router.refresh()` does.
    for (const write of CHAIN_WRITES) {
      expect(
        recorded.filter((route) => route === write),
        `${write} did not happen exactly once`,
      ).toHaveLength(1);
    }

    // ---- what each write actually carried ---------------------------------
    const find = (route: string) => after.moiraRequests.find((entry) => entry.route === route)!;

    // ======================================================================
    // AS THE HUMAN, NOT AS THE BOOTSTRAP KEY
    // ======================================================================
    //
    // `lib/moira-session.ts` gives `moiraClientForSession` no `systemKey`, and
    // says why: `MoiraClient`'s `admin` arm PREFERS the system key when both are
    // present, so a client built with one "would silently authenticate every
    // admin call as the bootstrap key instead of as the human — defeating the
    // audit trail and making the `admin_identities` grant untested in practice".
    //
    // This stack sets `MOIRA_SYSTEM_KEY` (a cold `consoleRuntime()` cannot
    // resolve an auth configuration without it), so that regression is one
    // constructor argument away here and it changes nothing a status code can
    // see. `credential` names which header arrived; `authenticated: boolean`,
    // which this replaced, was true for both.
    for (const entry of after.moiraRequests) {
      expect(
        entry.credential,
        `${entry.route} did not go out as the signed-in operator. "system_key" means the ` +
          "bootstrap credential was handed to a session-scoped client — see " +
          "moiraClientForSession in lib/moira-session.ts.",
      ).toBe("operator");
    }
    for (const write of CHAIN_WRITES) {
      expect(find(write).idempotencyKey, `${write} carried no Idempotency-Key`).not.toBeNull();
    }

    const providerBody = find("POST /api/v1/admin/providers").body as Record<string, unknown>;
    expect(providerBody["base_url"]).toBe(fixture.vllmBaseUrl);
    expect(providerBody["provider_type"]).toBe("open_ai_compatible");

    const modelBody = find(`POST /api/v1/admin/providers/${MOIRA_IDS.provider}/models`)
      .body as Record<string, unknown>;
    expect(modelBody["model_key"]).toBe(FIRST_MODEL);

    // The credential row exists even though the endpoint is keyless, and the
    // secret is the `api_key` arm and nothing else — an `endpoint` key present
    // (even as null) matches the azure arm of the untagged union too.
    const secret = (
      find("POST /api/v1/admin/provider-credentials").body as Record<string, unknown>
    )["secret"];
    expect(Object.keys(secret as object)).toEqual(["api_key"]);

    // Bound to the SEEDED route, and no route was created for it.
    const policyBody = find("POST /api/v1/admin/routing-policies").body as Record<string, unknown>;
    expect(policyBody["route_id"]).toBe(MOIRA_IDS.generalRoute);
    expect(policyBody["provider_id"]).toBe(MOIRA_IDS.provider);
    expect(policyBody["provider_model_id"]).toBe(MOIRA_IDS.providerModel);
    expect(recorded).not.toContain("POST /api/v1/admin/routes");

    // ---- and the operator is told, in the same order -----------------------
    //
    // Scoped to the shortcut's own `<section aria-label>` so the provider list
    // that appears after the refresh cannot contribute list items to this.
    const connectPanel = page.getByRole("region", {
      name: t(CONSOLE_MESSAGE_KEYS.llm_connect_heading),
    });
    await expect(connectPanel.getByText(t(CONSOLE_MESSAGE_KEYS.llm_trace_heading))).toBeVisible();
    const traceLabels = (await connectPanel.getByRole("listitem").allInnerTexts()).map((entry) =>
      entry.split("\n")[0]!.trim(),
    );
    expect(
      traceLabels,
      "the rendered trace does not name the five steps in the order they ran",
    ).toEqual([
      t(CONSOLE_MESSAGE_KEYS.llm_step_provider),
      t(CONSOLE_MESSAGE_KEYS.llm_step_provider_model),
      t(CONSOLE_MESSAGE_KEYS.llm_step_provider_credential),
      t(CONSOLE_MESSAGE_KEYS.llm_step_provider_enable),
      t(CONSOLE_MESSAGE_KEYS.llm_step_routing_policy),
    ]);

    // ---- the screen re-read what Moira says, not what it believed ----------
    // `LlmSettingsPanels` calls `router.refresh()` on success. Without it the
    // provider list still says "no providers" over a deployment that has one.
    await expect(
      page.getByRole("heading", { name: fixture.vllmBaseUrl, level: 3 }),
      "the provider list did not pick up the provider the chain just created",
    ).toBeVisible();
    await expect(page.getByText(t(CONSOLE_MESSAGE_KEYS.llm_providers_empty))).toHaveCount(0);

    expect(after.rows).toEqual({
      providers: 1,
      providerModels: 1,
      providerCredentials: 1,
      routingPolicies: 1,
    });
  });

  test("an unreachable endpoint is a keyed message, and nothing is left half-created", async ({
    page,
  }) => {
    await page.goto(LLM_PAGE, { waitUntil: "domcontentloaded" });
    await fillEndpoint(page, UNREACHABLE_ENDPOINT_URL);
    await resetRecording(page);
    await page
      .getByRole("button", { name: t(CONSOLE_MESSAGE_KEYS.llm_connect_discover_submit) })
      .click();

    // The catalog message for `console.llm.discovery_unreachable`, resolved
    // through the shipped `t()`. Asserting the RESOLVED copy rather than a
    // substring is what makes this a keyed-message assertion: a handler that
    // answered with a different key renders different words and fails, and a key
    // with no catalog entry renders the key itself and also fails.
    const alert = page.getByRole("alert").filter({
      hasText: t(CONSOLE_MESSAGE_KEYS.llm_discovery_unreachable),
    });
    await expect(alert).toBeVisible();
    await expect(alert).toHaveText(t(CONSOLE_MESSAGE_KEYS.llm_discovery_unreachable));

    // Nothing was offered, so there is nothing to press next.
    await expect(page.getByRole("checkbox")).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: t(CONSOLE_MESSAGE_KEYS.llm_connect_submit) }),
    ).toHaveCount(0);

    // ---- and NOTHING was created ------------------------------------------
    //
    // The half-created state this is guarding against is not hypothetical: the
    // chain's own 409 path exists because step three can fail after steps one
    // and two wrote. A discovery failure must not reach that path at all.
    const after = await readFixture();
    expect(
      writesIn(after.moiraRequests),
      "a failed discovery wrote to Moira. The chain's own 409 path exists because step three " +
        "can fail after steps one and two wrote; a discovery failure must not reach it at all.",
    ).toEqual([]);
    expect(after.rows).toEqual({
      providers: 0,
      providerModels: 0,
      providerCredentials: 0,
      routingPolicies: 0,
    });

    // The page still works: a laptop with the tunnel down is the normal case.
    await expect(
      page.getByRole("heading", { name: t(CONSOLE_MESSAGE_KEYS.llm_page_title), level: 1 }),
    ).toBeVisible();
  });

  test("no critical or serious axe violations on the signed-in page", async ({
    page,
  }, testInfo) => {
    const fixture = await readFixture();
    const response = await page.goto(LLM_PAGE, { waitUntil: "domcontentloaded" });
    expect(response!.status()).toBeLessThan(400);
    // The audited URL, asserted rather than assumed — `a11y.e2e.ts` audited
    // `/login` under three other routes' names for two waves by not doing this.
    expect(new URL(page.url()).pathname).toBe(LLM_PAGE);

    await runAxe(page, "initial", testInfo);

    // The discovered-models fieldset is a whole control group that exists only
    // after a successful probe, so the walker in `a11y.e2e.ts` could never have
    // seen it even with a session.
    await fillEndpoint(page, fixture.vllmOrigin);
    await page
      .getByRole("button", { name: t(CONSOLE_MESSAGE_KEYS.llm_connect_discover_submit) })
      .click();
    await expect(page.getByRole("checkbox", { name: FIRST_MODEL })).toBeChecked();

    await runAxe(page, "discovered", testInfo);
  });
});
