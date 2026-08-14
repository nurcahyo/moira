// The consumer-key mint form — the second place in this console where a live
// credential is handled.
//
// ============================================================================
// WHAT THESE TESTS ARE FOR
// ============================================================================
//
// Three properties, in order of how badly a regression would hurt:
//
//   1. THE KEY IS NOT BOUND HERE. The form holds the whole `MintedConsumerKey`
//      and hands `.secret` to the modal at the JSX site. That is a source-level
//      rule (`no-secret-props.test.ts` rule (c)) and this file asserts the
//      OBSERVABLE half: the key reaches the screen through the modal, and the
//      only component that receives it as a prop is that modal.
//   2. A REPLAY IS A SUCCESS. `secret: null` with `secret_retrievable: false` is
//      what Moira returns on an idempotent replay, and rendering it as a failure
//      would report a correct operation as broken on the retry path.
//   3. THE REQUEST SHAPE. `application_id`, the trimmed name, and the selected
//      scopes — because the server narrows the scopes but cannot invent the
//      application.

import { afterEach, describe, expect, test } from "bun:test";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import { MintKeyForm } from "@/modules/keys/MintKeyForm";

const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

const APPLICATION_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
/**
 * A fixed fake, so an assertion can look for this exact string.
 *
 * Deliberately low-entropy and self-describing. The first draft read like a real
 * key and `secret-scan` (gitleaks, which BLOCKS on this public repository) flagged
 * all three of this feature's fixtures. Teaching the allowlist to ignore the shape
 * of a Moira consumer key would be one more place a real one could hide, and the
 * tests need a recognisable string rather than a plausible credential.
 */
const MINTED_KEY = "moira_cons_example_not_a_real_key";

const OFFERED = ["moira:responses:create", "moira:responses:stream"] as const;

function mintResponse(secret: string | null): Response {
  return new Response(
    JSON.stringify({
      resource: {
        id: "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
        display_name: "Checkout service",
        key_prefix: "moira_cons_example",
        scopes: ["moira:responses:create"],
        status: "active",
        created_at: "2026-08-14T00:00:00Z",
        last_used_at: null,
        expires_at: null,
      },
      secret,
      secret_retrievable: secret !== null,
    }),
    { status: 201, headers: { "content-type": "application/json" } },
  );
}

function renderForm(fetchImpl: typeof fetch, onMinted?: () => void) {
  return render(
    <MintKeyForm
      applicationId={APPLICATION_ID}
      offeredScopes={OFFERED}
      defaultScope="moira:responses:create"
      fetchImpl={fetchImpl}
      {...(onMinted === undefined ? {} : { onMinted })}
    />,
  );
}

afterEach(cleanup);

describe("the mint request", () => {
  test("carries the application, the trimmed name and the checked scopes", async () => {
    const calls: Array<{ url: string; body: unknown }> = [];
    const fetchImpl = (async (url: string, init?: RequestInit) => {
      calls.push({ url: String(url), body: JSON.parse(String(init?.body)) });
      return mintResponse(MINTED_KEY);
    }) as unknown as typeof fetch;

    renderForm(fetchImpl);
    const user = userEvent.setup();
    await user.type(
      screen.getByRole("textbox", {
        name: new RegExp(copy(CONSOLE_MESSAGE_KEYS.keys_key_name_label)),
      }),
      "  Checkout service  ",
    );
    await user.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.keys_mint_button) }),
    );

    await waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0]?.url).toBe("/api/keys");
    expect(calls[0]?.body).toEqual({
      application_id: APPLICATION_ID,
      display_name: "Checkout service",
      scopes: ["moira:responses:create"],
    });
  });

  test("the default scope is checked and a second one can be added", async () => {
    const calls: Array<{ body: unknown }> = [];
    const fetchImpl = (async (_url: string, init?: RequestInit) => {
      calls.push({ body: JSON.parse(String(init?.body)) });
      return mintResponse(MINTED_KEY);
    }) as unknown as typeof fetch;

    renderForm(fetchImpl);
    const user = userEvent.setup();
    await user.type(
      screen.getByRole("textbox", {
        name: new RegExp(copy(CONSOLE_MESSAGE_KEYS.keys_key_name_label)),
      }),
      "Checkout",
    );
    await user.click(
      screen.getByRole("checkbox", {
        name: copy(CONSOLE_MESSAGE_KEYS.keys_scope_responses_stream),
      }),
    );
    await user.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.keys_mint_button) }),
    );

    await waitFor(() => expect(calls).toHaveLength(1));
    expect((calls[0]?.body as { scopes: string[] }).scopes.sort()).toEqual([
      "moira:responses:create",
      "moira:responses:stream",
    ]);
  });

  test("an empty name never reaches the network", async () => {
    let called = 0;
    const fetchImpl = (async () => {
      called += 1;
      return mintResponse(MINTED_KEY);
    }) as unknown as typeof fetch;

    renderForm(fetchImpl);
    const user = userEvent.setup();
    await user.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.keys_mint_button) }),
    );

    expect(called).toBe(0);
    expect(screen.getByRole("alert")).toHaveTextContent(
      copy(CONSOLE_MESSAGE_KEYS.keys_display_name_required),
    );
  });
});

describe("the minted key reaches the screen exactly once", () => {
  test("it is rendered, and the modal is what renders it", async () => {
    const fetchImpl = (async () => mintResponse(MINTED_KEY)) as unknown as typeof fetch;

    const { container } = renderForm(fetchImpl);
    const user = userEvent.setup();
    await user.type(
      screen.getByRole("textbox", {
        name: new RegExp(copy(CONSOLE_MESSAGE_KEYS.keys_key_name_label)),
      }),
      "Checkout",
    );
    await user.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.keys_mint_button) }),
    );

    await waitFor(() => expect(container.textContent).toContain(MINTED_KEY));
    // The console's own notice, not a fabricated Moira-shaped one: the key
    // envelope carries no `notice` field at all.
    expect(container.textContent).toContain(copy(CONSOLE_MESSAGE_KEYS.keys_secret_notice));
    // Labelled as a key rather than as an invitation token, heading included —
    // the modal's default heading reads "Invitation created", which is the first
    // thing an operator sees and the wrong words above a minted API key.
    expect(container.textContent).toContain(copy(CONSOLE_MESSAGE_KEYS.secret_key_label));
    expect(container.textContent).toContain(copy(CONSOLE_MESSAGE_KEYS.keys_secret_heading));
    expect(container.textContent).not.toContain(copy(CONSOLE_MESSAGE_KEYS.secret_modal_heading));
    // A consumer key has no redemption URL, so no link field is offered.
    expect(container.textContent).not.toContain(copy(CONSOLE_MESSAGE_KEYS.secret_link_label));
    // It does not expire, and the modal says so rather than leaving a blank.
    expect(container.textContent).toContain(copy(CONSOLE_MESSAGE_KEYS.secret_no_expiry));
  });

  test("a replay renders as already shown, NOT as a failure", async () => {
    const fetchImpl = (async () => mintResponse(null)) as unknown as typeof fetch;

    const { container } = renderForm(fetchImpl);
    const user = userEvent.setup();
    await user.type(
      screen.getByRole("textbox", {
        name: new RegExp(copy(CONSOLE_MESSAGE_KEYS.keys_key_name_label)),
      }),
      "Checkout",
    );
    await user.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.keys_mint_button) }),
    );

    await waitFor(() =>
      expect(container.textContent).toContain(copy(CONSOLE_MESSAGE_KEYS.secret_already_shown)),
    );
    expect(screen.queryByRole("alert")).toBeNull();
  });

  test("the caller is told to re-read the list", async () => {
    let reloads = 0;
    const fetchImpl = (async () => mintResponse(MINTED_KEY)) as unknown as typeof fetch;

    renderForm(fetchImpl, () => {
      reloads += 1;
    });
    const user = userEvent.setup();
    await user.type(
      screen.getByRole("textbox", {
        name: new RegExp(copy(CONSOLE_MESSAGE_KEYS.keys_key_name_label)),
      }),
      "Checkout",
    );
    await user.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.keys_mint_button) }),
    );

    await waitFor(() => expect(reloads).toBe(1));
  });
});

describe("failures are keyed, and never echo a body", () => {
  test("a refusal renders Moira's key through the catalog", async () => {
    const fetchImpl = (async () =>
      new Response(
        JSON.stringify({
          error: { message_key: CONSOLE_MESSAGE_KEYS.keys_application_required },
        }),
        { status: 400, headers: { "content-type": "application/json" } },
      )) as unknown as typeof fetch;

    renderForm(fetchImpl);
    const user = userEvent.setup();
    await user.type(
      screen.getByRole("textbox", {
        name: new RegExp(copy(CONSOLE_MESSAGE_KEYS.keys_key_name_label)),
      }),
      "Checkout",
    );
    await user.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.keys_mint_button) }),
    );

    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(
        copy(CONSOLE_MESSAGE_KEYS.keys_application_required),
      ),
    );
  });

  test("a transport failure is its own keyed message", async () => {
    const fetchImpl = (async () => {
      throw new Error("connection refused to https://user:pass@moira.invalid");
    }) as unknown as typeof fetch;

    renderForm(fetchImpl);
    const user = userEvent.setup();
    await user.type(
      screen.getByRole("textbox", {
        name: new RegExp(copy(CONSOLE_MESSAGE_KEYS.keys_key_name_label)),
      }),
      "Checkout",
    );
    await user.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.keys_mint_button) }),
    );

    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(
        copy(CONSOLE_MESSAGE_KEYS.keys_request_failed),
      ),
    );
    // The thrown cause is deliberately not read — it can carry a URL with
    // credentials in it.
    expect(document.body.textContent).not.toContain("user:pass");
  });
});
