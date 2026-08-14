// The issued-keys list, and the one destructive control on the keys screen.

import { afterEach, describe, expect, test } from "bun:test";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import type { ConsumerKeyView } from "@/lib/keys-view";
import { KeyList } from "@/modules/keys/KeyList";

const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

const KEY_ID = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

function key(overrides: Partial<ConsumerKeyView> = {}): ConsumerKeyView {
  return {
    id: KEY_ID,
    display_name: "Checkout service",
    key_prefix: "moira_cons_example",
    scopes: ["moira:responses:create"],
    status: "active",
    created_at: "2026-08-14T00:00:00Z",
    last_used_at: null,
    expires_at: null,
    ...overrides,
  };
}

const neverCalled = (async () => {
  throw new Error("the list must not call the network without a confirmation");
}) as unknown as typeof fetch;

afterEach(cleanup);

describe("what a row shows", () => {
  test("the prefix, and the absences stated rather than left blank", () => {
    const { container } = render(<KeyList keys={[key()]} fetchImpl={neverCalled} />);
    expect(container.textContent).toContain("moira_cons_example");
    expect(container.textContent).toContain(copy(CONSOLE_MESSAGE_KEYS.keys_never_used));
    expect(container.textContent).toContain(copy(CONSOLE_MESSAGE_KEYS.keys_expires_never));
  });

  test("a scope renders as copy, and an unknown one renders as itself", () => {
    // Rendering the raw string is the honest fallback HERE, unlike on the mint
    // form: a key minted through the admin API can carry a scope this screen
    // never offered, and hiding it would show the key as less capable than it is.
    const { container } = render(
      <KeyList
        keys={[key({ scopes: ["moira:responses:create", "moira:audit:read"] })]}
        fetchImpl={neverCalled}
      />,
    );
    expect(container.textContent).toContain(copy(CONSOLE_MESSAGE_KEYS.keys_scope_responses_create));
    expect(container.textContent).toContain("moira:audit:read");
  });

  test("the status is words, never colour alone", () => {
    const { container } = render(
      <KeyList keys={[key({ status: "revoked" })]} fetchImpl={neverCalled} />,
    );
    expect(container.textContent).toContain(copy(CONSOLE_MESSAGE_KEYS.keys_status_revoked));
  });

  test("an empty list says so rather than rendering nothing", () => {
    render(<KeyList keys={[]} fetchImpl={neverCalled} />);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.keys_issued_empty))).toBeDefined();
  });
});

describe("revoke asks first, and only where it can succeed", () => {
  test("a revoked key offers no revoke control", () => {
    // Moira would refuse it, and an offered control that cannot work is worse
    // than an absent one.
    render(<KeyList keys={[key({ status: "revoked" })]} fetchImpl={neverCalled} />);
    expect(
      screen.queryByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.keys_revoke_button) }),
    ).toBeNull();
  });

  test("the control opens a confirmation and sends nothing on its own", async () => {
    render(<KeyList keys={[key()]} fetchImpl={neverCalled} />);
    const user = userEvent.setup();
    await user.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.keys_revoke_button) }),
    );
    // The consequence sentence, which is the whole point of the dialog.
    expect(screen.getByRole("alert")).toHaveTextContent(
      copy(CONSOLE_MESSAGE_KEYS.keys_revoke_confirm_body),
    );
  });

  test("confirming posts to the console's own revoke endpoint and reloads", async () => {
    const calls: Array<{ url: string; method: string | undefined }> = [];
    const fetchImpl = (async (url: string, init?: RequestInit) => {
      calls.push({ url: String(url), method: init?.method });
      return new Response(JSON.stringify({ id: KEY_ID, status: "revoked" }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    }) as unknown as typeof fetch;

    let reloads = 0;
    render(
      <KeyList
        keys={[key()]}
        fetchImpl={fetchImpl}
        onRevoked={() => {
          reloads += 1;
        }}
      />,
    );
    const user = userEvent.setup();
    await user.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.keys_revoke_button) }),
    );
    // Two controls now carry that name — the row's and the dialog's. The dialog's
    // is the last one rendered.
    const controls = screen.getAllByRole("button", {
      name: copy(CONSOLE_MESSAGE_KEYS.keys_revoke_button),
    });
    await user.click(controls[controls.length - 1]!);

    await waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0]?.url).toBe(`/api/keys/${KEY_ID}/revoke`);
    expect(calls[0]?.method).toBe("POST");
    await waitFor(() => expect(reloads).toBe(1));
  });

  test("cancelling sends nothing", async () => {
    render(<KeyList keys={[key()]} fetchImpl={neverCalled} />);
    const user = userEvent.setup();
    await user.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.keys_revoke_button) }),
    );
    await user.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.action_cancel) }),
    );
    // `neverCalled` throws if the network is touched, so reaching this line is
    // the load-bearing half. The dialog is then closed — asserted by ROLE rather
    // than by text, because `Dialog` toggles the `open` attribute and leaves its
    // children in the DOM, so the confirmation copy is still queryable.
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  test("a refusal is keyed and the row survives", async () => {
    const fetchImpl = (async () =>
      new Response(JSON.stringify({ error: { message_key: "moira.error.conflict" } }), {
        status: 409,
        headers: { "content-type": "application/json" },
      })) as unknown as typeof fetch;

    render(<KeyList keys={[key()]} fetchImpl={fetchImpl} />);
    const user = userEvent.setup();
    await user.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.keys_revoke_button) }),
    );
    const controls = screen.getAllByRole("button", {
      name: copy(CONSOLE_MESSAGE_KEYS.keys_revoke_button),
    });
    await user.click(controls[controls.length - 1]!);

    await waitFor(() => expect(screen.getAllByRole("alert").length).toBeGreaterThan(0));
    expect(screen.getByText("Checkout service")).toBeDefined();
  });
});
